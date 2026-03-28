use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;
use std::time::Instant;

/// Benchmark: large rebase with many AI-authored commits
/// This simulates the real-world scenario reported by users in large monorepos
/// where rebases with AI authorship notes become extremely slow.
///
/// The test creates:
/// - A main branch that advances with N commits
/// - A feature branch with M commits, each touching AI-authored files
/// - Rebases the feature branch onto the advanced main branch
///
/// Run with: cargo test --package git-ai --test integration rebase_benchmark -- --ignored --nocapture
#[test]
#[ignore]
fn benchmark_rebase_many_ai_commits() {
    let num_feature_commits: usize = std::env::var("REBASE_BENCH_FEATURE_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let num_main_commits: usize = std::env::var("REBASE_BENCH_MAIN_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let num_ai_files: usize = std::env::var("REBASE_BENCH_AI_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let lines_per_file: usize = std::env::var("REBASE_BENCH_LINES_PER_FILE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);

    println!("\n=== Rebase Benchmark Configuration ===");
    println!("Feature commits: {}", num_feature_commits);
    println!("Main commits: {}", num_main_commits);
    println!("AI files per commit: {}", num_ai_files);
    println!("Lines per file: {}", lines_per_file);
    println!("=========================================\n");

    let repo = TestRepo::new();

    // Create initial commit on default branch
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    // Create feature branch with many AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let setup_start = Instant::now();

    for commit_idx in 0..num_feature_commits {
        // Each commit touches several AI-authored files
        for file_idx in 0..num_ai_files {
            let filename = format!("feature/module_{}/file_{}.rs", file_idx, file_idx);
            let mut file = repo.filename(&filename);

            // Build content with AI-authored lines that change each commit
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            for line_idx in 0..lines_per_file {
                let line_content = format!(
                    "// AI code v{} module {} line {}",
                    commit_idx, file_idx, line_idx
                );
                lines.push(line_content.ai());
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit(&format!("AI feature commit {}", commit_idx))
            .unwrap();

        if (commit_idx + 1) % 10 == 0 {
            println!(
                "  Created feature commit {}/{} ({:.1}s)",
                commit_idx + 1,
                num_feature_commits,
                setup_start.elapsed().as_secs_f64()
            );
        }
    }

    let feature_setup_time = setup_start.elapsed();
    println!(
        "Feature branch setup: {:.1}s ({} commits)",
        feature_setup_time.as_secs_f64(),
        num_feature_commits
    );

    // Advance main branch with non-conflicting commits
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_setup_start = Instant::now();

    for commit_idx in 0..num_main_commits {
        let filename = format!("main/change_{}.txt", commit_idx);
        let mut file = repo.filename(&filename);
        file.set_contents(crate::lines![format!("main content {}", commit_idx)]);
        repo.stage_all_and_commit(&format!("Main commit {}", commit_idx))
            .unwrap();
    }

    let main_setup_time = main_setup_start.elapsed();
    println!(
        "Main branch setup: {:.1}s ({} commits)",
        main_setup_time.as_secs_f64(),
        num_main_commits
    );

    // Now perform the rebase and measure time
    repo.git(&["checkout", "feature"]).unwrap();

    println!("\n--- Starting rebase ---");
    let rebase_start = Instant::now();
    let result = repo.git(&["rebase", &default_branch]);
    let rebase_duration = rebase_start.elapsed();

    match &result {
        Ok(output) => {
            println!("Rebase succeeded in {:.3}s", rebase_duration.as_secs_f64());
            println!("Output: {}", output);
        }
        Err(e) => {
            println!(
                "Rebase failed in {:.3}s: {}",
                rebase_duration.as_secs_f64(),
                e
            );
        }
    }
    result.unwrap();

    println!("\n=== BENCHMARK RESULTS ===");
    println!(
        "Total rebase time: {:.3}s ({:.0}ms)",
        rebase_duration.as_secs_f64(),
        rebase_duration.as_millis()
    );
    println!(
        "Per-commit average: {:.1}ms",
        rebase_duration.as_millis() as f64 / num_feature_commits as f64
    );
    println!("=========================\n");
}

/// Smaller benchmark for quick iteration during optimization
#[test]
#[ignore]
fn benchmark_rebase_small() {
    let num_commits = 10;
    let num_ai_files = 3;
    let lines_per_file = 20;

    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    for commit_idx in 0..num_commits {
        for file_idx in 0..num_ai_files {
            let filename = format!("feat/mod_{}/f_{}.rs", file_idx, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            for line_idx in 0..lines_per_file {
                lines.push(format!("// AI v{} m{} l{}", commit_idx, file_idx, line_idx).ai());
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit(&format!("feat {}", commit_idx))
            .unwrap();
    }

    repo.git(&["checkout", &default_branch]).unwrap();
    for i in 0..5 {
        let mut f = repo.filename(&format!("main_{}.txt", i));
        f.set_contents(crate::lines![format!("main {}", i)]);
        repo.stage_all_and_commit(&format!("main {}", i)).unwrap();
    }

    repo.git(&["checkout", "feature"]).unwrap();

    let start = Instant::now();
    repo.git(&["rebase", &default_branch]).unwrap();
    let dur = start.elapsed();

    println!("\n=== SMALL REBASE BENCHMARK ===");
    println!(
        "Commits: {}, AI files: {}, Lines/file: {}",
        num_commits, num_ai_files, lines_per_file
    );
    println!(
        "Total: {:.3}s ({:.0}ms)",
        dur.as_secs_f64(),
        dur.as_millis()
    );
    println!(
        "Per-commit: {:.1}ms",
        dur.as_millis() as f64 / num_commits as f64
    );
    println!("===============================\n");
}

/// Benchmark with performance JSON output for precise phase timing
#[test]
#[ignore]
fn benchmark_rebase_with_perf_json() {
    let num_commits: usize = std::env::var("REBASE_BENCH_FEATURE_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let num_ai_files: usize = std::env::var("REBASE_BENCH_AI_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);

    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();

    for commit_idx in 0..num_commits {
        for file_idx in 0..num_ai_files {
            let filename = format!("feat/mod_{}/f_{}.rs", file_idx, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            for line_idx in 0..30 {
                lines.push(
                    format!(
                        "// AI code v{} mod{} line{}",
                        commit_idx, file_idx, line_idx
                    )
                    .ai(),
                );
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit(&format!("feat {}", commit_idx))
            .unwrap();
    }

    repo.git(&["checkout", &default_branch]).unwrap();
    for i in 0..10 {
        let mut f = repo.filename(&format!("main_{}.txt", i));
        f.set_contents(crate::lines![format!("main {}", i)]);
        repo.stage_all_and_commit(&format!("main {}", i)).unwrap();
    }

    repo.git(&["checkout", "feature"]).unwrap();

    // Use benchmark_git to get performance JSON
    println!("\n--- Starting instrumented rebase ---");
    let start = Instant::now();
    let result = repo.benchmark_git(&["rebase", &default_branch]);
    let dur = start.elapsed();

    match result {
        Ok(bench) => {
            println!("\n=== INSTRUMENTED REBASE BENCHMARK ===");
            println!("Commits: {}, AI files: {}", num_commits, num_ai_files);
            println!("Total wall time: {:.3}s", dur.as_secs_f64());
            println!("Git duration: {:.3}s", bench.git_duration.as_secs_f64());
            println!(
                "Pre-command: {:.3}s",
                bench.pre_command_duration.as_secs_f64()
            );
            println!(
                "Post-command: {:.3}s",
                bench.post_command_duration.as_secs_f64()
            );
            println!(
                "Overhead: {:.3}s ({:.1}%)",
                (bench.total_duration - bench.git_duration).as_secs_f64(),
                ((bench.total_duration - bench.git_duration).as_millis() as f64
                    / bench.git_duration.as_millis().max(1) as f64)
                    * 100.0
            );
            println!("======================================\n");
        }
        Err(e) => {
            println!(
                "Benchmark result: {} (wall time: {:.3}s)",
                e,
                dur.as_secs_f64()
            );
            // Still useful even without structured perf data
        }
    }
}

/// Benchmark diff-based attribution transfer with large files and content changes.
/// This tests the scenario where rebasing changes file content (main branch modifies
/// AI-tracked files), forcing the diff-based path instead of the fast-path note remap.
///
/// Scale: 50 commits × 10 files × 200 lines = significant AI-authored content.
/// The diff-based path should complete the per-commit processing loop in <10ms total.
#[test]
#[ignore]
fn benchmark_rebase_diff_based_large() {
    let num_feature_commits: usize = std::env::var("REBASE_BENCH_FEATURE_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let num_ai_files: usize = std::env::var("REBASE_BENCH_AI_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let lines_per_file: usize = std::env::var("REBASE_BENCH_LINES_PER_FILE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    println!("\n=== Diff-Based Large Rebase Benchmark ===");
    println!("Feature commits: {}", num_feature_commits);
    println!("AI files: {}", num_ai_files);
    println!("Lines per file: {}", lines_per_file);
    println!("==========================================\n");

    let repo = TestRepo::new();

    // Create initial commit with shared files (both branches will modify)
    {
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            lines.push(format!("// Header for module {}", file_idx).into());
            lines.push("// Main branch will add lines above this marker".into());
            for line_idx in 0..lines_per_file {
                lines.push(format!("// Initial AI code mod{} line{}", file_idx, line_idx).ai());
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit("Initial shared files").unwrap();
    }

    let default_branch = repo.current_branch();

    // Create feature branch with AI commits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let setup_start = Instant::now();
    for commit_idx in 0..num_feature_commits {
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            let new_content = format!(
                "{}\n// AI addition v{} mod{}",
                current, commit_idx, file_idx
            );
            fs::write(&path, &new_content).unwrap();
            repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("AI feature {}", commit_idx))
            .unwrap();

        if (commit_idx + 1) % 10 == 0 {
            println!(
                "  Feature commit {}/{} ({:.1}s)",
                commit_idx + 1,
                num_feature_commits,
                setup_start.elapsed().as_secs_f64()
            );
        }
    }
    println!("Feature setup: {:.1}s", setup_start.elapsed().as_secs_f64());

    // Advance main branch with modifications to AI-tracked files (forces content changes on rebase)
    repo.git(&["checkout", &default_branch]).unwrap();
    for main_idx in 0..5 {
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            let new_content = current.replacen(
                "// Main branch will add lines above this marker",
                &format!(
                    "// Main addition {} for mod{}\n// Main branch will add lines above this marker",
                    main_idx, file_idx
                ),
                1,
            );
            fs::write(&path, &new_content).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("Main change {}", main_idx))
            .unwrap();
    }

    // Unrelated main commits
    for i in 0..10 {
        let filename = format!("main_only/change_{}.txt", i);
        let mut file = repo.filename(&filename);
        file.set_contents(crate::lines![format!("main only {}", i)]);
        repo.stage_all_and_commit(&format!("Main unrelated {}", i))
            .unwrap();
    }

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    let timing_file = repo.path().join("..").join("rebase_timing_diff.txt");
    let timing_path = timing_file.to_str().unwrap().to_string();

    println!("\n--- Starting diff-based rebase ---");
    let rebase_start = Instant::now();
    let result = repo.git_with_env(
        &["rebase", &default_branch],
        &[
            ("GIT_AI_DEBUG_PERFORMANCE", "1"),
            ("GIT_AI_REBASE_TIMING_FILE", &timing_path),
        ],
        None,
    );
    let rebase_duration = rebase_start.elapsed();

    match &result {
        Ok(_) => println!("Rebase succeeded in {:.3}s", rebase_duration.as_secs_f64()),
        Err(e) => println!(
            "Rebase FAILED in {:.3}s: {}",
            rebase_duration.as_secs_f64(),
            e
        ),
    }
    result.unwrap();

    if let Ok(timing_data) = fs::read_to_string(&timing_file) {
        println!("\n=== PHASE TIMING BREAKDOWN ===");
        print!("{}", timing_data);
        println!("===============================");
    }

    println!("\n=== DIFF-BASED LARGE BENCHMARK RESULTS ===");
    println!(
        "Total rebase time: {:.3}s ({:.0}ms)",
        rebase_duration.as_secs_f64(),
        rebase_duration.as_millis()
    );
    println!(
        "Per-commit average: {:.1}ms",
        rebase_duration.as_millis() as f64 / num_feature_commits as f64
    );
    println!("============================================\n");
}

/// Benchmark comparing the notes-based fast path vs blame-based slow path.
/// Runs the same rebase twice: once with notes (fast) and once without (blame fallback).
///
/// Run with: cargo test --test integration benchmark_blame_vs_diff -- --ignored --nocapture
#[test]
#[ignore]
fn benchmark_blame_vs_diff() {
    let num_feature_commits: usize = std::env::var("REBASE_BENCH_FEATURE_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let num_ai_files: usize = std::env::var("REBASE_BENCH_AI_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let lines_per_file: usize = std::env::var("REBASE_BENCH_LINES_PER_FILE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);

    println!("\n=== Blame vs Diff-Based Benchmark ===");
    println!("Feature commits: {}", num_feature_commits);
    println!("AI files: {}", num_ai_files);
    println!("Lines per file: {}", lines_per_file);
    println!("======================================\n");

    // Helper closure to create a test repo with the same setup
    let create_repo = |strip_notes: bool| -> (std::time::Duration, String) {
        let repo = TestRepo::new();
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            lines.push(format!("// Header for module {}", file_idx).into());
            lines.push("// Main branch marker".into());
            for line_idx in 0..lines_per_file {
                lines.push(format!("// AI code mod{} line{}", file_idx, line_idx).ai());
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit("Initial shared files").unwrap();
        let default_branch = repo.current_branch();

        repo.git(&["checkout", "-b", "feature"]).unwrap();
        for commit_idx in 0..num_feature_commits {
            for file_idx in 0..num_ai_files {
                let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
                let path = repo.path().join(&filename);
                let current = fs::read_to_string(&path).unwrap_or_default();
                let new_content = format!(
                    "{}\n// AI addition v{} mod{}",
                    current, commit_idx, file_idx
                );
                fs::write(&path, &new_content).unwrap();
                repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
            }
            repo.git(&["add", "-A"]).unwrap();
            repo.stage_all_and_commit(&format!("AI feature {}", commit_idx))
                .unwrap();
        }

        if strip_notes {
            // Delete the authorship notes ref to force the blame-based fallback
            let _ = repo.git(&["update-ref", "-d", "refs/notes/git-ai-authorship"]);
        }

        repo.git(&["checkout", &default_branch]).unwrap();
        for main_idx in 0..5 {
            for file_idx in 0..num_ai_files {
                let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
                let path = repo.path().join(&filename);
                let current = fs::read_to_string(&path).unwrap_or_default();
                let new_content = current.replacen(
                    "// Main branch marker",
                    &format!(
                        "// Main addition {} mod{}\n// Main branch marker",
                        main_idx, file_idx
                    ),
                    1,
                );
                fs::write(&path, &new_content).unwrap();
            }
            repo.git(&["add", "-A"]).unwrap();
            repo.stage_all_and_commit(&format!("Main {}", main_idx))
                .unwrap();
        }

        repo.git(&["checkout", "feature"]).unwrap();
        let timing_file = repo.path().join("..").join(if strip_notes {
            "timing_no_notes.txt"
        } else {
            "timing_with_notes.txt"
        });
        let timing_path = timing_file.to_str().unwrap().to_string();

        let rebase_start = Instant::now();
        repo.git_with_env(
            &["rebase", &default_branch],
            &[
                ("GIT_AI_DEBUG_PERFORMANCE", "1"),
                ("GIT_AI_REBASE_TIMING_FILE", &timing_path),
            ],
            None,
        )
        .unwrap();
        let duration = rebase_start.elapsed();

        let timing_data = fs::read_to_string(&timing_file).unwrap_or_default();
        (duration, timing_data)
    };

    // Run with notes (diff-based fast path)
    let (with_notes_dur, with_notes_timing) = create_repo(false);
    println!("--- WITH NOTES (diff-based path) ---");
    print!("{}", with_notes_timing);
    println!("Total rebase: {:.0}ms\n", with_notes_dur.as_millis());

    // Run without notes (blame-based slow path)
    let (no_notes_dur, no_notes_timing) = create_repo(true);
    println!("--- WITHOUT NOTES (blame-based fallback) ---");
    print!("{}", no_notes_timing);
    println!("Total rebase: {:.0}ms\n", no_notes_dur.as_millis());

    let authorship_with =
        extract_timing(&with_notes_timing, "TOTAL").unwrap_or(with_notes_dur.as_millis() as u64);
    let authorship_without =
        extract_timing(&no_notes_timing, "TOTAL").unwrap_or(no_notes_dur.as_millis() as u64);

    if authorship_without > 0 {
        let speedup = authorship_without as f64 / authorship_with.max(1) as f64;
        println!("=== COMPARISON ===");
        println!("Authorship rewrite with notes:    {}ms", authorship_with);
        println!("Authorship rewrite without notes: {}ms", authorship_without);
        println!("Speedup:                          {:.1}x", speedup);
        println!("==================\n");
    }
}

fn extract_timing(data: &str, key: &str) -> Option<u64> {
    for line in data.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(key)
            && let Some(val) = trimmed.split('=').nth(1)
        {
            return val.trim_end_matches("ms").parse().ok();
        }
    }
    None
}

/// Benchmark that forces the SLOW path (VirtualAttributions + blame) by having
/// main branch also modify AI-touched files. This causes blob differences
/// between original and rebased commits, making the fast-path note remap fail.
///
/// This is the worst-case scenario and what we need to optimize.
#[test]
#[ignore]
fn benchmark_rebase_slow_path() {
    let num_feature_commits: usize = std::env::var("REBASE_BENCH_FEATURE_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let num_ai_files: usize = std::env::var("REBASE_BENCH_AI_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let lines_per_file: usize = std::env::var("REBASE_BENCH_LINES_PER_FILE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    println!("\n=== Slow-Path Rebase Benchmark ===");
    println!("Feature commits: {}", num_feature_commits);
    println!("AI files: {}", num_ai_files);
    println!("Lines per file: {}", lines_per_file);
    println!("===================================\n");

    let repo = TestRepo::new();

    // Create initial commit with the shared files that both branches will modify
    // This ensures both branches touch the same AI-tracked files
    {
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            // Initial content: a header that main will modify + body that feature will modify
            lines.push(format!("// Header for module {}", file_idx).into());
            lines.push("// Main branch will add lines above this marker".into());
            for line_idx in 0..lines_per_file {
                lines.push(format!("// Initial AI code mod{} line{}", file_idx, line_idx).ai());
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit("Initial shared files").unwrap();
    }

    let default_branch = repo.current_branch();

    // Create feature branch with AI commits that modify the shared files
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    let setup_start = Instant::now();
    for commit_idx in 0..num_feature_commits {
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let path = repo.path().join(&filename);

            // Read current content and append AI lines at the bottom
            let current = fs::read_to_string(&path).unwrap_or_default();
            let new_content = format!(
                "{}\n// AI addition v{} mod{}",
                current, commit_idx, file_idx
            );
            fs::write(&path, &new_content).unwrap();

            // Checkpoint as AI
            repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("AI feature {}", commit_idx))
            .unwrap();

        if (commit_idx + 1) % 10 == 0 {
            println!(
                "  Feature commit {}/{} ({:.1}s)",
                commit_idx + 1,
                num_feature_commits,
                setup_start.elapsed().as_secs_f64()
            );
        }
    }
    println!("Feature setup: {:.1}s", setup_start.elapsed().as_secs_f64());

    // Go back to main and modify the SAME AI-tracked files at the TOP
    // This creates non-conflicting changes (different regions) that still cause
    // different blob OIDs after rebase, forcing the slow path
    repo.git(&["checkout", &default_branch]).unwrap();

    for main_idx in 0..5 {
        for file_idx in 0..num_ai_files {
            let filename = format!("shared/mod_{}/f_{}.rs", file_idx, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            // Insert at the top (before the marker)
            let new_content = current.replacen(
                "// Main branch will add lines above this marker",
                &format!(
                    "// Main addition {} for mod{}\n// Main branch will add lines above this marker",
                    main_idx, file_idx
                ),
                1,
            );
            fs::write(&path, &new_content).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("Main change {}", main_idx))
            .unwrap();
    }

    // Also add some unrelated main commits for realism
    for i in 0..10 {
        let filename = format!("main_only/change_{}.txt", i);
        let mut file = repo.filename(&filename);
        file.set_contents(crate::lines![format!("main only {}", i)]);
        repo.stage_all_and_commit(&format!("Main unrelated {}", i))
            .unwrap();
    }

    // Now rebase feature onto main - this should trigger the slow path
    // because the AI-tracked files have different blobs after rebase
    repo.git(&["checkout", "feature"]).unwrap();

    let timing_file = repo.path().join("..").join("rebase_timing.txt");
    let timing_path = timing_file.to_str().unwrap().to_string();

    println!("\n--- Starting slow-path rebase ---");
    let rebase_start = Instant::now();
    let result = repo.git_with_env(
        &["rebase", &default_branch],
        &[
            ("GIT_AI_DEBUG_PERFORMANCE", "1"),
            ("GIT_AI_REBASE_TIMING_FILE", &timing_path),
        ],
        None,
    );
    let rebase_duration = rebase_start.elapsed();

    match &result {
        Ok(output) => {
            println!("Rebase succeeded in {:.3}s", rebase_duration.as_secs_f64());
            // Print only last few lines of output to avoid noise
            let lines: Vec<&str> = output.lines().collect();
            let start = lines.len().saturating_sub(10);
            for line in &lines[start..] {
                println!("  {}", line);
            }
        }
        Err(e) => {
            println!(
                "Rebase FAILED in {:.3}s: {}",
                rebase_duration.as_secs_f64(),
                e
            );
        }
    }
    result.unwrap();

    // Read and display detailed timing breakdown
    if let Ok(timing_data) = fs::read_to_string(&timing_file) {
        println!("\n=== PHASE TIMING BREAKDOWN ===");
        print!("{}", timing_data);
        println!("===============================");
    }

    println!("\n=== SLOW-PATH BENCHMARK RESULTS ===");
    println!(
        "Total rebase time: {:.3}s ({:.0}ms)",
        rebase_duration.as_secs_f64(),
        rebase_duration.as_millis()
    );
    println!(
        "Per-commit average: {:.1}ms",
        rebase_duration.as_millis() as f64 / num_feature_commits as f64
    );
    println!("====================================\n");
}

/// Large-scale benchmark with mixed file sizes for PR comparison.
///
/// Creates:
/// - 200 AI-tracked files (150 × 1000 lines, 50 × 5000 lines)
/// - 150 feature commits, each modifying all files (ensuring AI attribution on every commit)
/// - Main branch also modifies the same files (forces diff-based path, not blob-copy fast path)
///
/// Run with: cargo test --package git-ai --test integration benchmark_large_scale_mixed -- --ignored --nocapture
#[test]
#[ignore]
fn benchmark_large_scale_mixed() {
    let num_small_files: usize = std::env::var("BENCH_SMALL_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let num_large_files: usize = std::env::var("BENCH_LARGE_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let small_file_lines: usize = std::env::var("BENCH_SMALL_LINES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let large_file_lines: usize = std::env::var("BENCH_LARGE_LINES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    let num_feature_commits: usize = std::env::var("BENCH_FEATURE_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let num_main_commits: usize = std::env::var("BENCH_MAIN_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let total_files = num_small_files + num_large_files;
    let total_initial_lines =
        num_small_files * small_file_lines + num_large_files * large_file_lines;

    println!("\n=== Large-Scale Mixed Benchmark ===");
    println!(
        "Small files: {} × {} lines",
        num_small_files, small_file_lines
    );
    println!(
        "Large files: {} × {} lines",
        num_large_files, large_file_lines
    );
    println!("Total files: {}", total_files);
    println!("Total initial lines: {}", total_initial_lines);
    println!("Feature commits: {}", num_feature_commits);
    println!("Main commits: {}", num_main_commits);
    println!("====================================\n");

    let repo = TestRepo::new();
    let setup_start = Instant::now();

    // Create initial commit with all files
    {
        for file_idx in 0..total_files {
            let lines_for_file = if file_idx < num_small_files {
                small_file_lines
            } else {
                large_file_lines
            };
            let filename = format!("src/mod_{}/file_{}.rs", file_idx % 20, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();
            lines.push(format!("// Module {} header", file_idx).into());
            lines.push("// MAIN_MARKER".into());
            for line_idx in 0..lines_for_file {
                lines.push(
                    format!(
                        "fn func_{}_{}() {{ /* AI generated */ }}",
                        file_idx, line_idx
                    )
                    .ai(),
                );
            }
            file.set_contents(lines);
        }
        repo.stage_all_and_commit("Initial: all AI files").unwrap();
    }
    println!(
        "Initial commit setup: {:.1}s",
        setup_start.elapsed().as_secs_f64()
    );

    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let feature_start = Instant::now();

    for commit_idx in 0..num_feature_commits {
        // Each commit modifies a subset of files (rotating window of ~20 files)
        // but touches enough to exercise the diff path
        let files_per_commit = 20.min(total_files);
        let start_file = (commit_idx * 7) % total_files; // rotating start to vary which files

        for i in 0..files_per_commit {
            let file_idx = (start_file + i) % total_files;
            let filename = format!("src/mod_{}/file_{}.rs", file_idx % 20, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            // Append AI-authored line at the end
            let new_content = format!(
                "{}\nfn feature_{}_in_{}() {{ /* AI commit {} */ }}",
                current, commit_idx, file_idx, commit_idx
            );
            fs::write(&path, &new_content).unwrap();
            repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("AI feature {}", commit_idx))
            .unwrap();

        if (commit_idx + 1) % 25 == 0 {
            println!(
                "  Feature commit {}/{} ({:.1}s)",
                commit_idx + 1,
                num_feature_commits,
                feature_start.elapsed().as_secs_f64()
            );
        }
    }
    println!(
        "Feature branch setup: {:.1}s ({} commits)",
        feature_start.elapsed().as_secs_f64(),
        num_feature_commits
    );

    // Advance main branch — modify AI-tracked files to force diff-based path
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_start = Instant::now();
    for main_idx in 0..num_main_commits {
        // Main modifies a different rotating set of files at the MARKER line
        let files_per_main = 30.min(total_files);
        let start_file = (main_idx * 13) % total_files;
        for i in 0..files_per_main {
            let file_idx = (start_file + i) % total_files;
            let filename = format!("src/mod_{}/file_{}.rs", file_idx % 20, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            let new_content = current.replacen(
                "// MAIN_MARKER",
                &format!(
                    "// Main change {} in file {}\n// MAIN_MARKER",
                    main_idx, file_idx
                ),
                1,
            );
            fs::write(&path, &new_content).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("Main {}", main_idx))
            .unwrap();
    }
    // Add unrelated main commits
    for i in 0..5 {
        let mut f = repo.filename(&format!("main_only/f_{}.txt", i));
        f.set_contents(crate::lines![format!("main only {}", i)]);
        repo.stage_all_and_commit(&format!("Main unrelated {}", i))
            .unwrap();
    }
    println!(
        "Main branch setup: {:.1}s",
        main_start.elapsed().as_secs_f64()
    );
    println!("Total setup: {:.1}s", setup_start.elapsed().as_secs_f64());

    // Rebase using benchmark_git for structured timing
    repo.git(&["checkout", "feature"]).unwrap();

    println!(
        "\n--- Starting rebase ({} commits onto {}) ---",
        num_feature_commits, &default_branch
    );
    let wall_start = Instant::now();
    let bench_result = repo.benchmark_git(&["rebase", &default_branch]);
    let wall_duration = wall_start.elapsed();

    match &bench_result {
        Ok(bench) => {
            let git_ms = bench.git_duration.as_millis();
            let total_ms = bench.total_duration.as_millis();
            let pre_ms = bench.pre_command_duration.as_millis();
            let post_ms = bench.post_command_duration.as_millis();
            let overhead_ms = total_ms.saturating_sub(git_ms);
            let overhead_pct = if git_ms > 0 {
                overhead_ms as f64 / git_ms as f64 * 100.0
            } else {
                0.0
            };

            println!("\n╔══════════════════════════════════════════════════════════╗");
            println!("║          LARGE-SCALE BENCHMARK RESULTS                  ║");
            println!("╠══════════════════════════════════════════════════════════╣");
            println!(
                "║  Files:          {} ({} × {}L + {} × {}L)",
                total_files, num_small_files, small_file_lines, num_large_files, large_file_lines
            );
            println!("║  Initial lines:  {}", total_initial_lines);
            println!("║  Commits:        {}", num_feature_commits);
            println!("╠══════════════════════════════════════════════════════════╣");
            println!("║  Wall time:      {:.3}s", wall_duration.as_secs_f64());
            println!("║  Total (wrapper): {}ms", total_ms);
            println!("║  Git rebase:     {}ms", git_ms);
            println!("║  Pre-command:    {}ms", pre_ms);
            println!("║  Post-command:   {}ms", post_ms);
            println!(
                "║  Overhead:       {}ms ({:.1}% of git time)",
                overhead_ms, overhead_pct
            );
            println!(
                "║  Per-commit avg: {:.1}ms total, {:.1}ms git, {:.1}ms overhead",
                total_ms as f64 / num_feature_commits as f64,
                git_ms as f64 / num_feature_commits as f64,
                overhead_ms as f64 / num_feature_commits as f64,
            );
            println!("╚══════════════════════════════════════════════════════════╝\n");
        }
        Err(e) => {
            println!(
                "Benchmark failed after {:.3}s: {}",
                wall_duration.as_secs_f64(),
                e
            );
            println!(
                "Wall time: {:.3}s ({:.0}ms)",
                wall_duration.as_secs_f64(),
                wall_duration.as_millis()
            );
            panic!("Benchmark failed: {}", e);
        }
    }
}

/// Brutal stress-test benchmark: hundreds of commits, dozens of files per commit,
/// hundreds to thousands of lines per file, 100% AI-attributed, varied edit patterns.
///
/// Each commit does a mix of:
/// - Multi-line insertions (5-20 lines) at random positions
/// - Line replacements (modify existing AI lines)
/// - Block deletions + re-additions
/// - New function/struct additions
///
/// Main branch also modifies AI-tracked files to force the diff-based path.
///
/// Run with: cargo test --test integration benchmark_brutal -- --ignored --nocapture
#[test]
#[ignore]
fn benchmark_brutal() {
    use crate::repos::test_repo::TestRepo;

    let num_files: usize = std::env::var("BRUTAL_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let num_feature_commits: usize = std::env::var("BRUTAL_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let num_main_commits: usize = std::env::var("BRUTAL_MAIN_COMMITS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let files_per_commit: usize = std::env::var("BRUTAL_FILES_PER_COMMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    // File sizes: 20 files × 2000 lines, 25 files × 500 lines, 15 files × 5000 lines
    let file_sizes: Vec<usize> = (0..num_files)
        .map(|i| {
            if i < 20 {
                2000
            } else if i < 45 {
                500
            } else {
                5000
            }
        })
        .collect();
    let total_initial_lines: usize = file_sizes.iter().sum();

    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║              BRUTAL REBASE BENCHMARK                    ║");
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  Files: {} (20×2000L + 25×500L + 15×5000L)", num_files);
    println!("║  Total initial lines: {}", total_initial_lines);
    println!("║  Feature commits: {}", num_feature_commits);
    println!("║  Files modified per commit: {}", files_per_commit);
    println!("║  Main commits: {}", num_main_commits);
    println!("║  All files 100% AI-attributed");
    println!("║  Varied edit patterns: insert, replace, delete, add");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    let repo = TestRepo::new();
    let setup_start = Instant::now();

    // Create initial commit: all files fully AI-attributed
    {
        for (file_idx, &line_count) in file_sizes.iter().enumerate() {
            let dir = match file_idx % 5 {
                0 => "src/api",
                1 => "src/models",
                2 => "src/services",
                3 => "src/utils",
                _ => "src/handlers",
            };
            let filename = format!("{}/module_{}.rs", dir, file_idx);
            let mut file = repo.filename(&filename);
            let mut lines: Vec<crate::repos::test_file::ExpectedLine> = Vec::new();

            // Realistic file structure
            lines.push(format!("// Module {} - auto-generated", file_idx).ai());
            lines.push("use std::collections::HashMap;".to_string().ai());
            lines.push("use serde::{{Serialize, Deserialize}};".to_string().ai());
            lines.push("// MAIN_MARKER".to_string().into());
            lines.push(format!("pub struct Module{} {{", file_idx).ai());

            for field_idx in 0..(line_count / 20).max(5) {
                lines.push(format!("    pub field_{}: String,", field_idx).ai());
            }
            lines.push("}".to_string().ai());
            lines.push(String::new().ai());

            // Fill rest with AI-authored functions
            let remaining = line_count.saturating_sub(lines.len());
            let funcs = remaining / 8;
            for func_idx in 0..funcs {
                lines.push(
                    format!(
                        "pub fn func_{}_{}(input: &str) -> String {{",
                        file_idx, func_idx
                    )
                    .ai(),
                );
                lines.push("    let result = input.to_uppercase();".to_string().ai());
                lines.push(
                    format!(
                        "    let processed = result.replace(\"old_{}\", \"new_{}\");",
                        func_idx, func_idx
                    )
                    .ai(),
                );
                lines.push(
                    format!(
                        "    // AI-generated logic for module {} func {}",
                        file_idx, func_idx
                    )
                    .ai(),
                );
                lines.push(
                    format!(
                        "    let output = format!(\"{{}}_{{}}\", processed, {});",
                        func_idx
                    )
                    .ai(),
                );
                lines.push(
                    format!(
                        "    log::debug!(\"func_{}_{}: {{}}\", &output);",
                        file_idx, func_idx
                    )
                    .ai(),
                );
                lines.push("    output".to_string().ai());
                lines.push("}".to_string().ai());
            }

            while lines.len() < line_count {
                lines.push(format!("// padding line {}", lines.len()).ai());
            }

            file.set_contents(lines);
        }
        repo.stage_all_and_commit("Initial: all AI-attributed files")
            .unwrap();
    }
    println!(
        "Initial commit: {:.1}s",
        setup_start.elapsed().as_secs_f64()
    );

    let default_branch = repo.current_branch();

    // Create feature branch with varied AI edits
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let feature_start = Instant::now();

    for commit_idx in 0..num_feature_commits {
        let stride = 3 + (commit_idx % 5);
        let start = (commit_idx * stride) % num_files;

        for i in 0..files_per_commit {
            let file_idx = (start + i * 2) % num_files;
            let dir = match file_idx % 5 {
                0 => "src/api",
                1 => "src/models",
                2 => "src/services",
                3 => "src/utils",
                _ => "src/handlers",
            };
            let filename = format!("{}/module_{}.rs", dir, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            let mut file_lines: Vec<&str> = current.lines().collect();

            let edit_type = (commit_idx * 7 + file_idx * 3) % 4;
            match edit_type {
                0 => {
                    // Multi-line insertion at varied position
                    let insert_pos = (file_lines.len() / 3)
                        + (commit_idx * 17 + file_idx * 11) % (file_lines.len() / 3).max(1);
                    let insert_pos = insert_pos.min(file_lines.len());
                    let num_insert = 5 + (commit_idx + file_idx) % 16;
                    let mut new_lines: Vec<String> = Vec::new();
                    for l in 0..num_insert {
                        new_lines.push(format!(
                            "    // AI insert c{} f{} line{}: processing batch {}",
                            commit_idx,
                            file_idx,
                            l,
                            commit_idx * 100 + l
                        ));
                    }
                    let new_refs: Vec<&str> = new_lines.iter().map(|s| s.as_str()).collect();
                    file_lines.splice(insert_pos..insert_pos, new_refs);
                    let joined = file_lines.join("\n");
                    fs::write(&path, &joined).unwrap();
                }
                1 => {
                    // Replace existing lines in middle third
                    let start_replace =
                        file_lines.len() / 4 + (commit_idx * 13) % (file_lines.len() / 4).max(1);
                    let num_replace = (3 + (commit_idx + file_idx) % 8)
                        .min(file_lines.len().saturating_sub(start_replace));
                    let mut new_lines: Vec<String> = Vec::new();
                    for l in 0..num_replace {
                        new_lines.push(format!(
                            "    let v{}_c{} = transform(input, {});",
                            l,
                            commit_idx,
                            commit_idx * 1000 + l
                        ));
                    }
                    let new_refs: Vec<&str> = new_lines.iter().map(|s| s.as_str()).collect();
                    let end = (start_replace + num_replace).min(file_lines.len());
                    file_lines.splice(start_replace..end, new_refs);
                    let joined = file_lines.join("\n");
                    fs::write(&path, &joined).unwrap();
                }
                2 => {
                    // Delete block + add new function at end
                    let del_start =
                        file_lines.len() / 2 + (commit_idx * 11) % (file_lines.len() / 6).max(1);
                    let del_count =
                        (2 + commit_idx % 5).min(file_lines.len().saturating_sub(del_start));
                    let end = (del_start + del_count).min(file_lines.len());
                    file_lines.drain(del_start..end);
                    let new_func = format!(
                        "\npub fn generated_{}_{}(x: i32) -> i32 {{\n    let a = x * {};\n    let b = a + {};\n    // AI logic c{} f{}\n    b\n}}",
                        commit_idx,
                        file_idx,
                        commit_idx + 1,
                        file_idx,
                        commit_idx,
                        file_idx
                    );
                    let joined = file_lines.join("\n") + &new_func;
                    fs::write(&path, &joined).unwrap();
                }
                _ => {
                    // Append struct + impl block
                    let block = format!(
                        "\n#[derive(Debug)]\nstruct Batch{}_{} {{\n    data: Vec<u8>,\n    count: usize,\n    tag: String,\n}}\n\nimpl Batch{}_{} {{\n    fn new() -> Self {{\n        Self {{ data: vec![], count: {}, tag: \"{}\".into() }}\n    }}\n    fn process(&self) -> usize {{ self.data.len() + self.count }}\n}}",
                        commit_idx, file_idx, commit_idx, file_idx, commit_idx, file_idx
                    );
                    let joined = file_lines.join("\n") + &block;
                    fs::write(&path, &joined).unwrap();
                }
            }

            repo.git_ai(&["checkpoint", "mock_ai", &filename]).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("Feature {}", commit_idx))
            .unwrap();

        if (commit_idx + 1) % 50 == 0 {
            println!(
                "  Feature commit {}/{} ({:.1}s)",
                commit_idx + 1,
                num_feature_commits,
                feature_start.elapsed().as_secs_f64()
            );
        }
    }
    println!(
        "Feature branch: {:.1}s ({} commits × {} files/commit)",
        feature_start.elapsed().as_secs_f64(),
        num_feature_commits,
        files_per_commit
    );

    // Advance main: modify AI-tracked files to force diff-based path
    repo.git(&["checkout", &default_branch]).unwrap();
    let main_start = Instant::now();
    for main_idx in 0..num_main_commits {
        let files_per_main = 40.min(num_files);
        let start_file = (main_idx * 11) % num_files;
        for i in 0..files_per_main {
            let file_idx = (start_file + i) % num_files;
            let dir = match file_idx % 5 {
                0 => "src/api",
                1 => "src/models",
                2 => "src/services",
                3 => "src/utils",
                _ => "src/handlers",
            };
            let filename = format!("{}/module_{}.rs", dir, file_idx);
            let path = repo.path().join(&filename);
            let current = fs::read_to_string(&path).unwrap_or_default();
            let new_content = current.replacen(
                "// MAIN_MARKER",
                &format!(
                    "use crate::main_dep_{}::*;\n// Main integration {} for module {}\n// MAIN_MARKER",
                    main_idx, main_idx, file_idx
                ),
                1,
            );
            fs::write(&path, &new_content).unwrap();
        }
        repo.git(&["add", "-A"]).unwrap();
        repo.stage_all_and_commit(&format!("Main {}", main_idx))
            .unwrap();
    }
    for i in 0..5 {
        let mut f = repo.filename(&format!("main_only/f_{}.txt", i));
        f.set_contents(crate::lines![format!("main only {}", i)]);
        repo.stage_all_and_commit(&format!("Main unrelated {}", i))
            .unwrap();
    }
    println!(
        "Main branch: {:.1}s ({} commits × {} files + 5 unrelated)",
        main_start.elapsed().as_secs_f64(),
        num_main_commits,
        40.min(num_files)
    );
    println!("Total setup: {:.1}s\n", setup_start.elapsed().as_secs_f64());

    // Rebase
    repo.git(&["checkout", "feature"]).unwrap();

    let timing_file = repo.path().join("..").join("brutal_timing.txt");
    let timing_path = timing_file.to_str().unwrap().to_string();

    println!(
        "--- Starting brutal rebase ({} commits onto {}) ---",
        num_feature_commits, &default_branch
    );
    let wall_start = Instant::now();
    let output = repo.git_with_env(
        &["rebase", &default_branch],
        &[
            ("GIT_AI_DEBUG_PERFORMANCE", "2"),
            ("GIT_AI_REBASE_TIMING_FILE", &timing_path),
        ],
        None,
    );
    let wall_duration = wall_start.elapsed();

    let timing_data = fs::read_to_string(&timing_file).unwrap_or_default();

    match &output {
        Ok(out) => {
            // Parse perf-json from output
            let bench = TestRepo::parse_benchmark_result(out);
            match bench {
                Ok(bench) => {
                    let git_ms = bench.git_duration.as_millis();
                    let total_ms = bench.total_duration.as_millis();
                    let pre_ms = bench.pre_command_duration.as_millis();
                    let post_ms = bench.post_command_duration.as_millis();
                    let overhead_ms = total_ms.saturating_sub(git_ms);
                    let overhead_pct = if git_ms > 0 {
                        overhead_ms as f64 / git_ms as f64 * 100.0
                    } else {
                        0.0
                    };

                    println!("\n╔══════════════════════════════════════════════════════════════╗");
                    println!("║              BRUTAL BENCHMARK RESULTS                        ║");
                    println!("╠══════════════════════════════════════════════════════════════╣");
                    println!("║  Scale:");
                    println!("║    {} files (20×2000L + 25×500L + 15×5000L)", num_files);
                    println!(
                        "║    {} initial lines of AI-attributed code",
                        total_initial_lines
                    );
                    println!(
                        "║    {} feature commits × {} files/commit",
                        num_feature_commits, files_per_commit
                    );
                    println!(
                        "║    {} main commits (forces diff-based path)",
                        num_main_commits
                    );
                    println!("╠══════════════════════════════════════════════════════════════╣");
                    println!("║  TIMING:");
                    println!(
                        "║    Wall clock:           {:.2}s",
                        wall_duration.as_secs_f64()
                    );
                    println!(
                        "║    git rebase (alone):   {}ms ({:.2}s)",
                        git_ms,
                        git_ms as f64 / 1000.0
                    );
                    println!(
                        "║    git-ai total:         {}ms ({:.2}s)",
                        total_ms,
                        total_ms as f64 / 1000.0
                    );
                    println!("║    ├─ pre-command:       {}ms", pre_ms);
                    println!(
                        "║    └─ post-command:      {}ms ({:.2}s)",
                        post_ms,
                        post_ms as f64 / 1000.0
                    );
                    println!("║");
                    println!("║  OVERHEAD:");
                    println!(
                        "║    git-ai overhead:      {}ms ({:.2}s)",
                        overhead_ms,
                        overhead_ms as f64 / 1000.0
                    );
                    println!("║    overhead / git time:  {:.1}%", overhead_pct);
                    println!(
                        "║    per-commit total:     {:.1}ms",
                        total_ms as f64 / num_feature_commits as f64
                    );
                    println!(
                        "║    per-commit git:       {:.1}ms",
                        git_ms as f64 / num_feature_commits as f64
                    );
                    println!(
                        "║    per-commit overhead:  {:.1}ms",
                        overhead_ms as f64 / num_feature_commits as f64
                    );
                    println!("╚══════════════════════════════════════════════════════════════╝");
                }
                Err(e) => {
                    println!(
                        "\nWall time: {:.2}s ({}ms) — perf-json parse failed: {}",
                        wall_duration.as_secs_f64(),
                        wall_duration.as_millis(),
                        e
                    );
                }
            }
        }
        Err(e) => {
            println!(
                "\nRebase FAILED after {:.2}s: {}",
                wall_duration.as_secs_f64(),
                e
            );
        }
    }
    output.unwrap();

    if !timing_data.is_empty() {
        println!("\n--- Internal phase timing ---");
        print!("{}", timing_data);
        println!("-----------------------------");
    }

    println!("\nDone.");
}
