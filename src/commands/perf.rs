use git_ai::observability::perf_regression;

/// Handle the `git-ai perf` subcommand.
///
/// Subcommands:
/// - `baseline` — capture current baseline from samples
/// - `status` — show current baselines and recent timings
/// - `reset` — clear all samples and baselines
pub fn handle_perf(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("baseline") => handle_baseline(),
        Some("status") => handle_status(),
        Some("reset") => handle_reset(),
        Some(cmd) => {
            eprintln!("git-ai perf: unknown subcommand '{}'", cmd);
            eprintln!("usage: git-ai perf <baseline|status|reset>");
            Err("unknown subcommand".into())
        }
        None => {
            println!("usage: git-ai perf <baseline|status|reset>");
            println!();
            println!("Subcommands:");
            println!("  baseline    Capture performance baseline from collected samples");
            println!("  status      Show current baselines and recent timing stats");
            println!("  reset       Clear all samples and baselines");
            Ok(())
        }
    }
}

fn handle_baseline() -> Result<(), Box<dyn std::error::Error>> {
    match perf_regression::capture_baseline() {
        Ok(()) => {
            println!("Baseline captured successfully.");
            // Show what was captured
            if let Ok(baseline) = perf_regression::load_baseline() {
                println!();
                println!("{:<20} {:>8} {:>8} {:>8}", "OPERATION", "P50(ms)", "P95(ms)", "SAMPLES");
                println!("{}", "-".repeat(50));
                let mut ops: Vec<_> = baseline.0.iter().collect();
                ops.sort_by_key(|(name, _)| name.clone());
                for (op, stats) in ops {
                    println!(
                        "{:<20} {:>8.2} {:>8.2} {:>8}",
                        op, stats.p50_ms, stats.p95_ms, stats.samples
                    );
                }
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to capture baseline: {}", e);
            Err(e.into())
        }
    }
}

fn handle_status() -> Result<(), Box<dyn std::error::Error>> {
    // Show baseline
    println!("=== Performance Baseline ===");
    match perf_regression::load_baseline() {
        Ok(baseline) if !baseline.0.is_empty() => {
            println!("{:<20} {:>8} {:>8} {:>8}", "OPERATION", "P50(ms)", "P95(ms)", "SAMPLES");
            println!("{}", "-".repeat(50));
            let mut ops: Vec<_> = baseline.0.iter().collect();
            ops.sort_by_key(|(name, _)| name.clone());
            for (op, stats) in ops {
                println!(
                    "{:<20} {:>8.2} {:>8.2} {:>8}",
                    op, stats.p50_ms, stats.p95_ms, stats.samples
                );
            }
        }
        Ok(_) => {
            println!("  (no baseline captured yet)");
        }
        Err(e) => {
            println!("  (no baseline: {})", e);
        }
    }

    println!();
    println!("=== Recent Samples ===");
    match perf_regression::load_samples() {
        Ok(samples) if !samples.0.is_empty() => {
            println!("{:<20} {:>8} {:>8} {:>8} {:>8}", "OPERATION", "COUNT", "MIN(ms)", "MAX(ms)", "AVG(ms)");
            println!("{}", "-".repeat(58));
            let mut ops: Vec<_> = samples.0.iter().collect();
            ops.sort_by_key(|(name, _)| name.clone());
            for (op, timings) in ops {
                if timings.is_empty() {
                    continue;
                }
                let min = timings.iter().cloned().fold(f64::INFINITY, f64::min);
                let max = timings.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let avg = timings.iter().sum::<f64>() / timings.len() as f64;
                println!(
                    "{:<20} {:>8} {:>8.2} {:>8.2} {:>8.2}",
                    op,
                    timings.len(),
                    min,
                    max,
                    avg
                );
            }
        }
        Ok(_) => {
            println!("  (no samples collected yet)");
        }
        Err(e) => {
            println!("  (no samples: {})", e);
        }
    }

    Ok(())
}

fn handle_reset() -> Result<(), Box<dyn std::error::Error>> {
    perf_regression::reset_all().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    println!("All performance samples and baselines have been cleared.");
    Ok(())
}
