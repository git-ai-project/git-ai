#[macro_use]
mod repos;

use rand::{Rng, distributions::Alphanumeric};
use repos::test_repo::TestRepo;
use std::{fs, time::Instant};

#[test]
fn test_checkpoint_size_logging_large_ai_rewrites() {
    eprintln!("test_checkpoint_size_logging_large_ai_rewrites started...");
    let repo = TestRepo::new();
    let mut rng = rand::thread_rng();

    // (target_lines, iterations)
    let configs: &[(usize, usize)] = &[
        (2, 5),
        (20, 5),
        (200, 5),
        (500, 5),
        (1000, 5),
        // (2_000, 5), // uncomment for heavier run
    ];

    let file_path = repo.path().join("large_ai_file.txt");

    for (config_idx, (target_lines, iterations)) in configs.iter().copied().enumerate() {
        eprintln!("config {config_idx}: target_lines={target_lines}, iterations={iterations}");

        let mut durations = Vec::with_capacity(iterations);

        for iteration in 0..iterations {
            // Build a fresh file with random AI-authored content for this iteration.
            let mut content = String::with_capacity(target_lines * 48);
            for line_idx in 0..target_lines {
                let random_fragment: String =
                    (0..24).map(|_| rng.sample(Alphanumeric) as char).collect();
                content.push_str(&format!(
                    "ai_line_{config_idx}_{iteration}_{line_idx}_{random_fragment}\n"
                ));
            }

            eprintln!("config {config_idx} iteration {iteration} (starting checkpoint)");

            let start = Instant::now();
            fs::write(&file_path, &content).expect("should write large file");

            // Mark the entire rewrite as AI-authored for this iteration.
            let git_ai_output = repo
                .git_ai(&["checkpoint", "mock_ai", "large_ai_file.txt"])
                .expect("git-ai checkpoint should succeed");

            eprintln!("git-ai checkpoint output:\n{git_ai_output}\n");

            durations.push(start.elapsed());

            eprintln!(
                "config {config_idx} iteration {iteration} duration: {} ms",
                start.elapsed().as_millis()
            );
        }

        let mut sorted = durations.clone();
        sorted.sort();
        let median = sorted[sorted.len() / 2];
        let max = sorted[sorted.len() - 1];

        for (idx, duration) in durations.iter().enumerate() {
            println!(
                "config {config_idx} iteration {idx}: {} ms",
                duration.as_millis()
            );
        }
        println!(
            "config {config_idx} median duration: {} ms, max duration: {} ms",
            median.as_millis(),
            max.as_millis()
        );

        let working_log = repo.current_working_logs();
        let checkpoints_file = working_log.dir.join("checkpoints.jsonl");
        let size = fs::metadata(&checkpoints_file)
            .expect("checkpoints.jsonl should exist")
            .len();

        println!(
            "config {config_idx} checkpoints.jsonl path: {:?}, size (bytes): {}",
            checkpoints_file, size
        );
    }
}

worktree_test_wrappers! {
    test_checkpoint_size_logging_large_ai_rewrites,
}
