//! Benchmarks for the bash tool stat-snapshot and diff system.
//!
//! Measures snapshot() and diff() performance across synthetic repos of varying sizes.
//!
//! | Repo Size | Files   | Target P95 |
//! |-----------|---------|------------|
//! | Small     | 1,000   | < 10ms     |
//! | Medium    | 10,000  | < 50ms     |
//! | Large     | 100,000 | < 500ms    |
//! | XLarge    | 500,000 | < 5s       |
//!
//! Run with: cargo test bash_tool_benchmark --release -- --nocapture --ignored

use git_ai::commands::checkpoint_agent::bash_tool;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Statistics helpers
// ---------------------------------------------------------------------------

/// Timing data for one iteration of a snapshot + diff cycle.
#[derive(Debug, Clone)]
struct IterationTiming {
    snapshot_duration: Duration,
    diff_duration: Duration,
}

/// Descriptive statistics for a set of duration measurements.
#[derive(Debug)]
struct DurationStats {
    count: usize,
    min: Duration,
    max: Duration,
    average: Duration,
    p95: Duration,
    std_dev_ms: f64,
}

impl DurationStats {
    fn from_durations(durations: &[Duration]) -> Self {
        let count = durations.len();
        assert!(count > 0, "cannot compute stats from empty slice");

        let total: Duration = durations.iter().sum();
        let average = total / count as u32;
        let min = *durations.iter().min().unwrap();
        let max = *durations.iter().max().unwrap();

        // P95: sort and pick the value at the 95th-percentile index.
        let mut sorted: Vec<Duration> = durations.to_vec();
        sorted.sort();
        let p95_index = ((count as f64) * 0.95).ceil() as usize - 1;
        let p95 = sorted[p95_index.min(count - 1)];

        // Standard deviation in milliseconds.
        let avg_ms = average.as_secs_f64() * 1000.0;
        let variance: f64 = durations
            .iter()
            .map(|d| {
                let ms = d.as_secs_f64() * 1000.0;
                (ms - avg_ms).powi(2)
            })
            .sum::<f64>()
            / count as f64;
        let std_dev_ms = variance.sqrt();

        Self {
            count,
            min,
            max,
            average,
            p95,
            std_dev_ms,
        }
    }

    fn print(&self, label: &str) {
        println!("\n=== {} ({} runs) ===", label, self.count);
        println!("  Min:      {:.2}ms", self.min.as_secs_f64() * 1000.0);
        println!("  Average:  {:.2}ms", self.average.as_secs_f64() * 1000.0);
        println!("  Max:      {:.2}ms", self.max.as_secs_f64() * 1000.0);
        println!("  P95:      {:.2}ms", self.p95.as_secs_f64() * 1000.0);
        println!("  Std Dev:  {:.2}ms", self.std_dev_ms);
    }
}

// ---------------------------------------------------------------------------
// Synthetic repo construction
// ---------------------------------------------------------------------------

/// Create a temporary git repo at `root` containing `file_count` files spread
/// across a nested directory tree.  Files are grouped into directories of at
/// most ~100 files each, with up to 3 levels of nesting for realism.
fn create_synthetic_repo(root: &Path, file_count: usize) {
    fs::create_dir_all(root).expect("failed to create repo root");

    // git init
    let output = Command::new("git")
        .args(["init"])
        .current_dir(root)
        .output()
        .expect("git init failed");
    assert!(output.status.success(), "git init failed");

    // Configure user for commits
    for (key, val) in [
        ("user.name", "Bench User"),
        ("user.email", "bench@test.com"),
    ] {
        let output = Command::new("git")
            .args(["config", key, val])
            .current_dir(root)
            .output()
            .expect("git config failed");
        assert!(output.status.success(), "git config {} failed", key);
    }

    // Create a .gitignore to mimic real repos (ignore build artifacts, etc.)
    fs::write(root.join(".gitignore"), "target/\nnode_modules/\n*.o\n")
        .expect("failed to write .gitignore");

    // Build a nested directory tree.
    // Strategy: files_per_dir ~= 100, dirs are nested up to 3 levels.
    let files_per_dir: usize = 100;
    let total_dirs = file_count.div_ceil(files_per_dir);

    let mut files_created: usize = 0;
    for dir_index in 0..total_dirs {
        // Compute a nested path: level0/level1/level2
        let l0 = dir_index % 50;
        let l1 = (dir_index / 50) % 50;
        let l2 = dir_index / 2500;
        let dir_path = root
            .join(format!("src_{}", l2))
            .join(format!("mod_{}", l1))
            .join(format!("pkg_{}", l0));
        fs::create_dir_all(&dir_path).expect("failed to create nested dir");

        let remaining = file_count - files_created;
        let batch = remaining.min(files_per_dir);
        for file_index in 0..batch {
            let filename = format!("file_{}.rs", file_index);
            let content = format!(
                "// auto-generated benchmark file {}/{}\nfn f{}() {{}}\n",
                dir_index,
                file_index,
                files_created + file_index
            );
            fs::write(dir_path.join(&filename), content).expect("failed to write file");
        }
        files_created += batch;
    }

    assert_eq!(
        files_created, file_count,
        "expected to create {} files, created {}",
        file_count, files_created
    );

    // Stage and commit everything.  For large repos, `git add -A` followed by
    // a single commit is the fastest approach.
    let add_output = Command::new("git")
        .args(["add", "-A"])
        .current_dir(root)
        .output()
        .expect("git add failed");
    assert!(add_output.status.success(), "git add -A failed");

    let commit_output = Command::new("git")
        .args(["commit", "-m", "initial synthetic commit"])
        .current_dir(root)
        .output()
        .expect("git commit failed");
    assert!(
        commit_output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Benchmark harness
// ---------------------------------------------------------------------------

const NUM_ITERATIONS: usize = 5;

/// Run `NUM_ITERATIONS` of snapshot + diff on the given repo root.
/// Returns (snapshot_stats, diff_stats).
fn run_benchmark(repo_root: &Path, label: &str) -> (DurationStats, DurationStats) {
    println!(
        "\n--- {} benchmark ({} iterations) ---",
        label, NUM_ITERATIONS
    );

    let mut timings: Vec<IterationTiming> = Vec::with_capacity(NUM_ITERATIONS);

    for i in 1..=NUM_ITERATIONS {
        // Take a pre-snapshot
        let snap_start = Instant::now();
        let pre = bash_tool::snapshot(repo_root, "bench-session", &format!("pre-{}", i), None)
            .expect("pre-snapshot should succeed");
        let snapshot_duration = snap_start.elapsed();

        // Modify a single file to make the diff non-trivial
        let marker_path = repo_root.join("bench_marker.txt");
        fs::write(&marker_path, format!("iteration {}", i)).expect("failed to write marker");

        // Take a post-snapshot
        let post = bash_tool::snapshot(repo_root, "bench-session", &format!("post-{}", i), None)
            .expect("post-snapshot should succeed");

        // Diff the two snapshots
        let diff_start = Instant::now();
        let diff_result = bash_tool::diff(&pre, &post);
        let diff_duration = diff_start.elapsed();

        // Sanity check: the marker file should show up as created or modified
        assert!(
            !diff_result.is_empty(),
            "diff should detect marker file change"
        );

        println!(
            "  Iteration {}: snapshot={:.2}ms (entries={}), diff={:.2}ms (created={}, modified={})",
            i,
            snapshot_duration.as_secs_f64() * 1000.0,
            pre.entries.len(),
            diff_duration.as_secs_f64() * 1000.0,
            diff_result.created.len(),
            diff_result.modified.len(),
        );

        timings.push(IterationTiming {
            snapshot_duration,
            diff_duration,
        });

        // Clean up marker for next iteration
        let _ = fs::remove_file(&marker_path);
    }

    let snap_durations: Vec<Duration> = timings.iter().map(|t| t.snapshot_duration).collect();
    let diff_durations: Vec<Duration> = timings.iter().map(|t| t.diff_duration).collect();

    let snap_stats = DurationStats::from_durations(&snap_durations);
    let diff_stats = DurationStats::from_durations(&diff_durations);

    snap_stats.print(&format!("{} Snapshot", label));
    diff_stats.print(&format!("{} Diff", label));

    (snap_stats, diff_stats)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
#[ignore]
fn test_bash_tool_snapshot_benchmark_small() {
    const FILE_COUNT: usize = 1_000;
    const TARGET_P95_MS: f64 = 10.0;
    // CI margin: 10x the target to account for slow CI runners
    const CI_MARGIN: f64 = 10.0;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("small_repo");

    println!("\n========================================");
    println!("Bash Tool Benchmark: SMALL ({} files)", FILE_COUNT);
    println!("Target P95: < {}ms", TARGET_P95_MS);
    println!("========================================");

    let setup_start = Instant::now();
    create_synthetic_repo(&repo_root, FILE_COUNT);
    println!(
        "Repo setup: {:.2}ms",
        setup_start.elapsed().as_secs_f64() * 1000.0
    );

    let (snap_stats, _diff_stats) = run_benchmark(&repo_root, "Small (1K)");

    let p95_ms = snap_stats.p95.as_secs_f64() * 1000.0;
    println!(
        "\nSmall repo P95: {:.2}ms (target: {}ms, CI limit: {}ms)",
        p95_ms,
        TARGET_P95_MS,
        TARGET_P95_MS * CI_MARGIN,
    );
    assert!(
        p95_ms < TARGET_P95_MS * CI_MARGIN,
        "Small repo snapshot P95 ({:.2}ms) exceeded CI limit ({}ms)",
        p95_ms,
        TARGET_P95_MS * CI_MARGIN,
    );
}

#[test]
#[ignore]
fn test_bash_tool_snapshot_benchmark_medium() {
    const FILE_COUNT: usize = 10_000;
    const TARGET_P95_MS: f64 = 50.0;
    const CI_MARGIN: f64 = 10.0;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("medium_repo");

    println!("\n========================================");
    println!("Bash Tool Benchmark: MEDIUM ({} files)", FILE_COUNT);
    println!("Target P95: < {}ms", TARGET_P95_MS);
    println!("========================================");

    let setup_start = Instant::now();
    create_synthetic_repo(&repo_root, FILE_COUNT);
    println!(
        "Repo setup: {:.2}ms",
        setup_start.elapsed().as_secs_f64() * 1000.0
    );

    let (snap_stats, _diff_stats) = run_benchmark(&repo_root, "Medium (10K)");

    let p95_ms = snap_stats.p95.as_secs_f64() * 1000.0;
    println!(
        "\nMedium repo P95: {:.2}ms (target: {}ms, CI limit: {}ms)",
        p95_ms,
        TARGET_P95_MS,
        TARGET_P95_MS * CI_MARGIN,
    );
    assert!(
        p95_ms < TARGET_P95_MS * CI_MARGIN,
        "Medium repo snapshot P95 ({:.2}ms) exceeded CI limit ({}ms)",
        p95_ms,
        TARGET_P95_MS * CI_MARGIN,
    );
}

#[test]
#[ignore]
fn test_bash_tool_snapshot_benchmark_large() {
    const FILE_COUNT: usize = 100_000;
    const TARGET_P95_MS: f64 = 500.0;
    const CI_MARGIN: f64 = 10.0;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("large_repo");

    println!("\n========================================");
    println!("Bash Tool Benchmark: LARGE ({} files)", FILE_COUNT);
    println!("Target P95: < {}ms", TARGET_P95_MS);
    println!("========================================");

    let setup_start = Instant::now();
    create_synthetic_repo(&repo_root, FILE_COUNT);
    println!("Repo setup: {:.2}s", setup_start.elapsed().as_secs_f64());

    let (snap_stats, _diff_stats) = run_benchmark(&repo_root, "Large (100K)");

    let p95_ms = snap_stats.p95.as_secs_f64() * 1000.0;
    println!(
        "\nLarge repo P95: {:.2}ms (target: {}ms, CI limit: {}ms)",
        p95_ms,
        TARGET_P95_MS,
        TARGET_P95_MS * CI_MARGIN,
    );
    assert!(
        p95_ms < TARGET_P95_MS * CI_MARGIN,
        "Large repo snapshot P95 ({:.2}ms) exceeded CI limit ({}ms)",
        p95_ms,
        TARGET_P95_MS * CI_MARGIN,
    );
}

#[test]
#[ignore]
fn test_bash_tool_snapshot_benchmark_xlarge() {
    // This test creates 500K files and is too slow for CI.  It validates
    // graceful degradation: the snapshot function should either succeed within
    // the 5-second timeout or return an error about exceeding MAX_TRACKED_FILES.
    const FILE_COUNT: usize = 500_000;
    const TARGET_P95_MS: f64 = 5_000.0;
    const CI_MARGIN: f64 = 4.0;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("xlarge_repo");

    println!("\n========================================");
    println!("Bash Tool Benchmark: XLARGE ({} files)", FILE_COUNT);
    println!(
        "Target P95: < {}ms (with graceful degradation)",
        TARGET_P95_MS
    );
    println!("WARNING: This test creates 500K files and may take several minutes to set up.");
    println!("========================================");

    let setup_start = Instant::now();
    create_synthetic_repo(&repo_root, FILE_COUNT);
    println!("Repo setup: {:.2}s", setup_start.elapsed().as_secs_f64());

    // For XLarge we run fewer iterations since setup is so expensive.
    println!("\n--- XLarge benchmark (3 iterations) ---");
    let mut snapshot_durations: Vec<Duration> = Vec::new();

    for i in 1..=3 {
        let snap_start = Instant::now();
        let result = bash_tool::snapshot(&repo_root, "bench-session", &format!("xl-{}", i), None);
        let elapsed = snap_start.elapsed();

        match result {
            Ok(snap) => {
                println!(
                    "  Iteration {}: snapshot={:.2}ms (entries={})",
                    i,
                    elapsed.as_secs_f64() * 1000.0,
                    snap.entries.len(),
                );
                snapshot_durations.push(elapsed);
            }
            Err(e) => {
                // Graceful degradation: the function may reject repos above
                // MAX_TRACKED_FILES.  That is acceptable behavior.
                println!(
                    "  Iteration {}: snapshot returned error after {:.2}ms -- {}",
                    i,
                    elapsed.as_secs_f64() * 1000.0,
                    e,
                );
                // Verify the rejection was fast (should not spin for ages).
                assert!(
                    elapsed < Duration::from_secs(10),
                    "Graceful degradation should be fast; took {:.2}s",
                    elapsed.as_secs_f64(),
                );
                println!("  (graceful degradation confirmed)");
                return; // No further timing assertions needed
            }
        }
    }

    if !snapshot_durations.is_empty() {
        let stats = DurationStats::from_durations(&snapshot_durations);
        stats.print("XLarge (500K) Snapshot");

        let p95_ms = stats.p95.as_secs_f64() * 1000.0;
        println!(
            "\nXLarge repo P95: {:.2}ms (target: {}ms, CI limit: {}ms)",
            p95_ms,
            TARGET_P95_MS,
            TARGET_P95_MS * CI_MARGIN,
        );
        // Softer assertion: just warn instead of failing hard since this is
        // expected to be slow.
        if p95_ms > TARGET_P95_MS {
            println!(
                "WARNING: XLarge P95 ({:.2}ms) exceeded ideal target ({}ms) -- acceptable for large repos",
                p95_ms, TARGET_P95_MS,
            );
        }
        assert!(
            p95_ms < TARGET_P95_MS * CI_MARGIN,
            "XLarge repo snapshot P95 ({:.2}ms) exceeded CI limit ({}ms)",
            p95_ms,
            TARGET_P95_MS * CI_MARGIN,
        );
    }
}

#[test]
#[ignore]
fn test_bash_tool_diff_performance() {
    // Benchmarks the diff() function in isolation by building two large
    // in-memory snapshots and diffing them.
    const FILE_COUNT: usize = 10_000;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("diff_bench_repo");

    println!("\n========================================");
    println!("Bash Tool Diff-Only Benchmark ({} files)", FILE_COUNT);
    println!("========================================");

    create_synthetic_repo(&repo_root, FILE_COUNT);

    // Take a baseline snapshot.
    let pre =
        bash_tool::snapshot(&repo_root, "diff-bench", "pre", None).expect("pre-snapshot should succeed");

    // Modify 1% of files to simulate realistic edits.
    let files_to_modify = FILE_COUNT / 100;
    let mut modified_count = 0;
    let mut dirs_to_visit = vec![repo_root.clone()];
    'outer: while let Some(dir) = dirs_to_visit.pop() {
        if dir.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        let entries = fs::read_dir(&dir).expect("failed to read dir");
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs_to_visit.push(path);
            } else if path.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                fs::write(
                    &path,
                    format!("// modified\nfn modified_{}() {{}}\n", modified_count),
                )
                .expect("failed to modify file");
                modified_count += 1;
                if modified_count >= files_to_modify {
                    break 'outer;
                }
            }
        }
    }
    println!("Modified {} files for diff benchmark", modified_count);

    // Take a post-snapshot.
    let post = bash_tool::snapshot(&repo_root, "diff-bench", "post", None)
        .expect("post-snapshot should succeed");

    // Benchmark diff() over multiple iterations.
    println!(
        "\n--- Diff-only benchmark ({} iterations) ---",
        NUM_ITERATIONS
    );
    let mut diff_durations: Vec<Duration> = Vec::with_capacity(NUM_ITERATIONS);

    for i in 1..=NUM_ITERATIONS {
        let start = Instant::now();
        let result = bash_tool::diff(&pre, &post);
        let elapsed = start.elapsed();

        println!(
            "  Iteration {}: diff={:.4}ms (created={}, modified={})",
            i,
            elapsed.as_secs_f64() * 1000.0,
            result.created.len(),
            result.modified.len(),
        );

        // Sanity: we should see roughly the number of files we modified.
        assert!(
            result.modified.len() >= modified_count / 2,
            "Expected at least {} modified files, got {}",
            modified_count / 2,
            result.modified.len(),
        );

        diff_durations.push(elapsed);
    }

    let stats = DurationStats::from_durations(&diff_durations);
    stats.print("Diff-Only (10K files, 1% modified)");

    // Diff should be very fast since it is purely in-memory HashSet operations.
    let p95_ms = stats.p95.as_secs_f64() * 1000.0;
    assert!(
        p95_ms < 50.0,
        "Diff P95 ({:.2}ms) should be under 50ms for 10K entries",
        p95_ms,
    );
}

#[test]
#[ignore]
fn test_bash_tool_git_status_fallback_benchmark() {
    // Benchmarks git_status_fallback() which shells out to `git status`.
    const FILE_COUNT: usize = 10_000;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("fallback_bench_repo");

    println!("\n========================================");
    println!(
        "Bash Tool git_status_fallback Benchmark ({} files)",
        FILE_COUNT
    );
    println!("========================================");

    create_synthetic_repo(&repo_root, FILE_COUNT);

    // Create some uncommitted changes so git status has something to report.
    fs::write(repo_root.join("new_file.txt"), "new content").expect("failed to write new file");
    let modify_target = repo_root
        .join("src_0")
        .join("mod_0")
        .join("pkg_0")
        .join("file_0.rs");
    if modify_target.exists() {
        fs::write(&modify_target, "// modified\n").expect("failed to modify file");
    }

    println!(
        "\n--- git_status_fallback benchmark ({} iterations) ---",
        NUM_ITERATIONS
    );
    let mut durations: Vec<Duration> = Vec::with_capacity(NUM_ITERATIONS);

    for i in 1..=NUM_ITERATIONS {
        let start = Instant::now();
        let result =
            bash_tool::git_status_fallback(&repo_root).expect("git_status_fallback should succeed");
        let elapsed = start.elapsed();

        println!(
            "  Iteration {}: {:.2}ms ({} changed files)",
            i,
            elapsed.as_secs_f64() * 1000.0,
            result.len(),
        );

        assert!(
            !result.is_empty(),
            "git_status_fallback should detect uncommitted changes"
        );

        durations.push(elapsed);
    }

    let stats = DurationStats::from_durations(&durations);
    stats.print("git_status_fallback (10K files)");
}

#[test]
#[ignore]
fn test_bash_tool_snapshot_entry_count_accuracy() {
    // Verify that the snapshot captures the expected number of files.
    const FILE_COUNT: usize = 1_000;

    let tmp = tempfile::tempdir().expect("failed to create tempdir");
    let repo_root = tmp.path().join("accuracy_repo");

    println!("\n========================================");
    println!("Bash Tool Snapshot Accuracy ({} files)", FILE_COUNT);
    println!("========================================");

    create_synthetic_repo(&repo_root, FILE_COUNT);

    let snap =
        bash_tool::snapshot(&repo_root, "accuracy", "check", None).expect("snapshot should succeed");

    // The snapshot should contain at least FILE_COUNT entries (the .rs files)
    // plus the .gitignore.  It may contain more if the walker picks up
    // additional metadata files.
    let entry_count = snap.entries.len();
    println!("Snapshot entries: {}", entry_count);

    assert!(
        entry_count >= FILE_COUNT,
        "Expected at least {} snapshot entries, got {}",
        FILE_COUNT,
        entry_count,
    );

    // Verify nested directory structure is preserved in paths.
    let has_nested = snap.entries.keys().any(|p| p.components().count() >= 3);
    assert!(
        has_nested,
        "Expected snapshot to contain paths with at least 3 levels of nesting"
    );
}
