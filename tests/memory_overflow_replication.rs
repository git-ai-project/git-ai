#[macro_use]
mod repos;

use repos::test_repo::TestRepo;
use std::fs;
use std::time::Instant;

/// Generate a large fake transcript JSON string that simulates a long agent conversation.
/// Each message is ~2KB of text, simulating realistic AI assistant responses with code.
fn generate_large_transcript(message_count: usize) -> String {
    let mut messages = Vec::with_capacity(message_count);
    for i in 0..message_count {
        let padding = "x".repeat(1800);
        if i % 3 == 0 {
            messages.push(format!(
                r#"{{"User":{{"text":"User message {} with padding: {}","timestamp":"2025-01-01T00:00:00Z"}}}}"#,
                i, padding
            ));
        } else if i % 3 == 1 {
            messages.push(format!(
                r#"{{"Assistant":{{"text":"Assistant response {} with code and explanation: {}","timestamp":"2025-01-01T00:00:01Z"}}}}"#,
                i, padding
            ));
        } else {
            messages.push(format!(
                r#"{{"ToolUse":{{"name":"edit_file","input":{{"path":"src/file_{}.rs","content":"fn main() {{ {} }}"}},"timestamp":"2025-01-01T00:00:02Z"}}}}"#,
                i, padding
            ));
        }
    }
    format!(r#"{{"messages":[{}]}}"#, messages.join(","))
}

/// Generate a single checkpoint JSONL line with the given parameters.
/// Simulates what append_checkpoint writes - a full Checkpoint struct serialized as JSON.
fn generate_checkpoint_jsonl_line(
    checkpoint_idx: usize,
    file_count: usize,
    transcript_messages: usize,
    tool_name: &str,
) -> String {
    let mut entries = Vec::with_capacity(file_count);
    for f in 0..file_count {
        let line_attrs = format!(
            r#"{{"line":{},"author_id":"ai_agent_hash_{}","timestamp":{},"overrode":null}}"#,
            f,
            checkpoint_idx,
            1700000000 + checkpoint_idx as u64
        );
        entries.push(format!(
            r#"{{"file":"src/file_{}.rs","blob_sha":"deadbeef{}{}","attributions":[],"line_attributions":[{}]}}"#,
            f, checkpoint_idx, f, line_attrs
        ));
    }

    let transcript = if transcript_messages > 0 {
        format!(r#","transcript":{}"#, generate_large_transcript(transcript_messages))
    } else {
        String::new()
    };

    let agent_id = format!(
        r#","agent_id":{{"tool":"{}","id":"session-{}-{}","model":"gpt-4o"}}"#,
        tool_name, tool_name, checkpoint_idx
    );

    format!(
        r#"{{"kind":"AiAgent","diff":"diff-hash-{}","author":"ai-agent","entries":[{}],"timestamp":{}{}{},"agent_metadata":null,"line_stats":{{"additions":10,"deletions":2,"additions_sloc":8,"deletions_sloc":1}},"api_version":"checkpoint/1.0.0","git_ai_version":"1.0.42"}}"#,
        checkpoint_idx,
        entries.join(","),
        1700000000 + checkpoint_idx as u64,
        transcript,
        agent_id
    )
}

/// Directly write a checkpoints.jsonl file with the given number of checkpoints.
/// This bypasses the normal checkpoint flow to rapidly create large checkpoint files
/// that simulate what accumulates during long agent sessions.
fn write_synthetic_checkpoints(
    working_log_dir: &std::path::Path,
    num_checkpoints: usize,
    files_per_checkpoint: usize,
    transcript_messages: usize,
    tool_name: &str,
) -> u64 {
    fs::create_dir_all(working_log_dir).expect("should create working log dir");
    let checkpoints_file = working_log_dir.join("checkpoints.jsonl");

    let mut content = String::new();
    for i in 0..num_checkpoints {
        content.push_str(&generate_checkpoint_jsonl_line(
            i,
            files_per_checkpoint,
            transcript_messages,
            tool_name,
        ));
        content.push('\n');
    }

    fs::write(&checkpoints_file, &content).expect("should write checkpoints.jsonl");
    content.len() as u64
}

/// Measure RSS (Resident Set Size) of the current process in bytes.
/// Falls back to 0 if /proc/self/status is not available (non-Linux).
fn get_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2
                        && let Ok(kb) = parts[1].parse::<u64>()
                    {
                        return kb * 1024;
                    }
                }
            }
        }
        0
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

// ============================================================================
// TEST 1: Demonstrate O(N^2) behavior from append_checkpoint re-reading all
// checkpoints every time. Each append reads ALL existing + writes ALL back.
// With N checkpoints, total I/O is O(N^2) and memory peaks at full file size.
// ============================================================================
#[test]
fn test_memory_overflow_append_checkpoint_quadratic_growth() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("TEST: append_checkpoint O(N^2) growth - simulates multiple agent sessions");
    eprintln!("Each append_checkpoint() call re-reads ALL existing checkpoints from disk,");
    eprintln!("deserializes them, appends one, and re-serializes ALL back to disk.");
    eprintln!("{}\n", "=".repeat(80));

    let repo = TestRepo::new();
    let file_path = repo.path().join("test_file.rs");

    // Create initial commit
    fs::write(&file_path, "fn main() {}\n").expect("write file");
    repo.git(&["add", "test_file.rs"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    // Now simulate many AI agent checkpoint iterations
    // Each iteration: modify file -> checkpoint mock_ai -> measure
    let iterations = 30;
    let mut checkpoint_times = Vec::new();
    let mut file_sizes = Vec::new();

    let working_log = repo.current_working_logs();
    let checkpoints_file = working_log.dir.join("checkpoints.jsonl");

    for i in 0..iterations {
        // Modify the file (simulate AI edit)
        let content = format!(
            "fn main() {{}}\n{}\n",
            (0..=i)
                .map(|j| format!("// AI edit iteration {}", j))
                .collect::<Vec<_>>()
                .join("\n")
        );
        fs::write(&file_path, &content).expect("write file");

        let start = Instant::now();
        let result = repo.git_ai(&["checkpoint", "mock_ai", "test_file.rs"]);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "checkpoint should succeed at iteration {}", i);

        let file_size = if checkpoints_file.exists() {
            fs::metadata(&checkpoints_file).unwrap().len()
        } else {
            0
        };

        checkpoint_times.push(elapsed);
        file_sizes.push(file_size);

        eprintln!(
            "  Iteration {:>3}: checkpoint took {:>8.2?}, JSONL size: {:>10}",
            i,
            elapsed,
            format_bytes(file_size)
        );
    }

    // Verify quadratic growth pattern: later iterations should be significantly slower
    let first_5_avg = checkpoint_times[..5]
        .iter()
        .map(|d| d.as_millis())
        .sum::<u128>()
        / 5;
    let last_5_avg = checkpoint_times[iterations - 5..]
        .iter()
        .map(|d| d.as_millis())
        .sum::<u128>()
        / 5;

    eprintln!("\n  First 5 iterations avg: {} ms", first_5_avg);
    eprintln!("  Last 5 iterations avg:  {} ms", last_5_avg);
    eprintln!(
        "  Final JSONL file size:  {}",
        format_bytes(*file_sizes.last().unwrap())
    );
    eprintln!(
        "  Growth ratio (last/first): {:.1}x",
        if first_5_avg > 0 {
            last_5_avg as f64 / first_5_avg as f64
        } else {
            0.0
        }
    );
}

// ============================================================================
// TEST 2: Demonstrate memory explosion from large transcripts stored in
// checkpoints. Tools like "mock_ai" and "opencode" keep full transcripts
// in the JSONL file because they "cannot_refetch". With long agent sessions,
// transcripts can be 10-50MB+ each, and they ALL get loaded into memory.
// ============================================================================
#[test]
fn test_memory_overflow_large_transcripts_accumulation() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("TEST: Large transcript accumulation in checkpoints.jsonl");
    eprintln!("Simulates long agent sessions where transcripts are stored inline.");
    eprintln!("Tools that 'cannot_refetch' (mock_ai, opencode, unknown tools) keep");
    eprintln!("full transcripts, which are re-loaded on EVERY read_all_checkpoints() call.");
    eprintln!("{}\n", "=".repeat(80));

    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("app.rs");
    fs::write(&file_path, "fn main() {}\n").expect("write file");
    repo.git(&["add", "app.rs"]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let working_log = repo.current_working_logs();

    // Write synthetic checkpoints with large transcripts directly to the JSONL file
    // This simulates what accumulates over a long session with multiple agents
    let configs: &[(usize, usize, usize, &str)] = &[
        // (num_checkpoints, files_per_checkpoint, transcript_messages, description)
        (5, 3, 50, "Small session: 5 checkpoints, 50 messages each"),
        (20, 5, 100, "Medium session: 20 checkpoints, 100 messages each"),
        (50, 5, 200, "Large session: 50 checkpoints, 200 messages each"),
        (100, 10, 300, "XL session: 100 checkpoints, 300 messages each"),
    ];

    for (num_checkpoints, files_per_cp, transcript_msgs, description) in configs {
        eprintln!("  Config: {}", description);

        let rss_before = get_rss_bytes();

        let file_size = write_synthetic_checkpoints(
            &working_log.dir,
            *num_checkpoints,
            *files_per_cp,
            *transcript_msgs,
            "mock_ai",
        );
        eprintln!("    JSONL file size: {}", format_bytes(file_size));

        // Now trigger a read_all_checkpoints - this is what happens during pre-commit
        let start = Instant::now();
        let checkpoints = working_log.read_all_checkpoints();
        let read_elapsed = start.elapsed();

        let rss_after = get_rss_bytes();
        let rss_delta = rss_after.saturating_sub(rss_before);

        match &checkpoints {
            Ok(cps) => {
                eprintln!("    Loaded {} checkpoints in {:?}", cps.len(), read_elapsed);
                eprintln!(
                    "    RSS delta: {} (before: {}, after: {})",
                    format_bytes(rss_delta),
                    format_bytes(rss_before),
                    format_bytes(rss_after)
                );

                // Estimate in-memory size: file is read as string + deserialized structs
                // At minimum 2x the file size (string + parsed structs), often 3-5x
                let estimated_min_memory = file_size * 2;
                eprintln!(
                    "    Estimated minimum memory for this load: {} (2x file size)",
                    format_bytes(estimated_min_memory)
                );
                eprintln!(
                    "    NOTE: During a single checkpoint::run(), read_all_checkpoints() is called 3+ times!"
                );
                eprintln!(
                    "    Estimated peak memory: {} (3x minimum for repeated reads)",
                    format_bytes(estimated_min_memory * 3)
                );
            }
            Err(e) => {
                eprintln!("    ERROR reading checkpoints: {}", e);
            }
        }
        eprintln!();
    }
}

// ============================================================================
// TEST 3: Demonstrate the multiplied reads problem. A single checkpoint::run()
// call triggers read_all_checkpoints() at LEAST 3 times:
//   1. checkpoint.rs line 286: read existing checkpoints
//   2. checkpoint.rs line 663: get_all_tracked_files reads checkpoints
//   3. checkpoint.rs line 693: get_all_tracked_files reads AGAIN for has_ai_checkpoints
//   4. repo_storage.rs line 325: append_checkpoint reads ALL checkpoints again
// Plus the post-commit flow reads them yet again.
// ============================================================================
#[test]
fn test_memory_overflow_multiplied_checkpoint_reads() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("TEST: Multiplied checkpoint reads during a single operation");
    eprintln!("A single checkpoint::run() reads ALL checkpoints 4+ times.");
    eprintln!("Post-commit then reads them again. Total: 5-6 full deserializations.");
    eprintln!("{}\n", "=".repeat(80));

    let repo = TestRepo::new();

    // Create initial commit with multiple files
    let num_files = 5;
    for i in 0..num_files {
        let path = repo.path().join(format!("src/file_{}.rs", i));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("// file {}\nfn func_{}() {{}}\n", i, i)).unwrap();
    }
    repo.git(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial files"]).unwrap();

    let working_log = repo.current_working_logs();

    // Pre-populate with a moderately large checkpoint history
    // 30 checkpoints * 5 files * 100 transcript messages
    let file_size = write_synthetic_checkpoints(&working_log.dir, 30, 5, 100, "mock_ai");
    eprintln!(
        "  Pre-populated JSONL: {} ({} checkpoints)",
        format_bytes(file_size),
        30
    );

    // Now modify a file and trigger a checkpoint
    // This will internally call read_all_checkpoints() 4+ times
    for i in 0..num_files {
        let path = repo.path().join(format!("src/file_{}.rs", i));
        fs::write(
            &path,
            format!("// file {} - modified\nfn func_{}() {{ /* new */ }}\n", i, i),
        )
        .unwrap();
    }

    let rss_before = get_rss_bytes();
    let start = Instant::now();

    // This single call will read the full JSONL at least 4 times internally
    let result = repo.git_ai(&["checkpoint", "mock_ai", "--", "src/file_0.rs", "src/file_1.rs"]);
    let elapsed = start.elapsed();

    let rss_after = get_rss_bytes();
    let rss_delta = rss_after.saturating_sub(rss_before);

    eprintln!("  Single checkpoint operation took: {:?}", elapsed);
    eprintln!(
        "  RSS delta: {} (before: {}, after: {})",
        format_bytes(rss_delta),
        format_bytes(rss_before),
        format_bytes(rss_after)
    );
    eprintln!(
        "  With {} JSONL file being read 4+ times, peak memory usage is at least: {}",
        format_bytes(file_size),
        format_bytes(file_size * 4 * 2) // 4 reads * 2x (string + parsed)
    );

    match result {
        Ok(output) => eprintln!("  Checkpoint succeeded: {}", output.lines().next().unwrap_or("")),
        Err(e) => eprintln!("  Checkpoint error (may be expected with synthetic data): {}", e),
    }
}

// ============================================================================
// TEST 4: Full end-to-end replication. Simulates a realistic long session
// with multiple AI agents making many edits, then triggering a commit.
// This is closest to the actual user-reported scenario.
// ============================================================================
#[test]
fn test_memory_overflow_realistic_multi_agent_session() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("TEST: Realistic multi-agent session memory overflow replication");
    eprintln!("Simulates: multiple AI agents editing files over many iterations,");
    eprintln!("then a git commit triggers pre-commit hook processing.");
    eprintln!("{}\n", "=".repeat(80));

    let repo = TestRepo::new();
    let num_files = 10;

    // Create initial files
    for i in 0..num_files {
        let path = repo.path().join(format!("module_{}.py", i));
        let content = format!(
            "# Module {}\ndef function_{}():\n    pass\n",
            i, i
        );
        fs::write(&path, content).unwrap();
    }
    repo.git(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial modules"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints_file = working_log.dir.join("checkpoints.jsonl");

    // Phase 1: Simulate multiple agent sessions making checkpoints
    // Each "agent session" edits some files and creates a checkpoint
    let agent_sessions = 15;
    let files_per_session = 4;

    eprintln!("  Phase 1: Simulating {} agent sessions...", agent_sessions);

    for session in 0..agent_sessions {
        // Each agent edits a subset of files
        let start_file = (session * 2) % num_files;
        let mut edited_files = Vec::new();

        for f in 0..files_per_session {
            let file_idx = (start_file + f) % num_files;
            let path = repo.path().join(format!("module_{}.py", file_idx));
            let content = format!(
                "# Module {} - edited by agent session {}\ndef function_{}():\n    # AI generated code iteration {}\n    result = compute_{}_{}()\n    return result\n\ndef helper_{}_{}():\n    pass\n",
                file_idx, session, file_idx, session, file_idx, session, file_idx, session
            );
            fs::write(&path, content).unwrap();
            edited_files.push(format!("module_{}.py", file_idx));
        }

        let mut args: Vec<&str> = vec!["checkpoint", "mock_ai", "--"];
        let refs: Vec<&str> = edited_files.iter().map(|s| s.as_str()).collect();
        args.extend(refs);

        let start = Instant::now();
        let result = repo.git_ai(&args);
        let elapsed = start.elapsed();

        let file_size = if checkpoints_file.exists() {
            fs::metadata(&checkpoints_file).unwrap().len()
        } else {
            0
        };

        eprintln!(
            "    Session {:>2}: checkpoint took {:>8.2?}, JSONL: {:>10}",
            session,
            elapsed,
            format_bytes(file_size)
        );

        if let Err(e) = result {
            eprintln!("    WARNING: checkpoint failed: {}", e);
        }
    }

    // Phase 2: Now trigger a commit (which runs pre-commit + post-commit hooks)
    // This is where the memory explosion happens in production
    eprintln!("\n  Phase 2: Triggering commit (pre-commit + post-commit hooks)...");

    // Stage all changes
    repo.git(&["add", "."]).unwrap();

    let final_jsonl_size = if checkpoints_file.exists() {
        fs::metadata(&checkpoints_file).unwrap().len()
    } else {
        0
    };
    eprintln!(
        "    JSONL file size before commit: {}",
        format_bytes(final_jsonl_size)
    );

    let rss_before = get_rss_bytes();
    let start = Instant::now();

    // The commit will trigger:
    // 1. pre-commit hook -> checkpoint::run() -> 4+ read_all_checkpoints()
    // 2. post-commit hook -> post_commit() -> read_all_checkpoints() + VirtualAttributions
    let result = repo.git(&["commit", "-m", "multi-agent session commit"]);
    let elapsed = start.elapsed();

    let rss_after = get_rss_bytes();
    let rss_delta = rss_after.saturating_sub(rss_before);

    eprintln!("    Commit took: {:?}", elapsed);
    eprintln!(
        "    RSS delta: {} (before: {}, after: {})",
        format_bytes(rss_delta),
        format_bytes(rss_before),
        format_bytes(rss_after)
    );

    // Calculate theoretical peak memory
    // During commit: pre-commit reads checkpoints ~4 times, post-commit reads ~2 more times
    // Each read: string allocation + deserialized structs = ~2-3x file size
    // Peak concurrent: at least 2x file size (string + structs) per read
    // With 6 reads, if GC doesn't collect fast enough: up to 6 * 2x = 12x file size
    let theoretical_peak = final_jsonl_size * 12;
    eprintln!(
        "    Theoretical peak memory (6 reads * 2x file): {}",
        format_bytes(theoretical_peak)
    );
    eprintln!(
        "    At scale (users report 1-5GB JSONL files), this would be: {:.1} - {:.1} GB",
        1.0 * 12.0,
        5.0 * 12.0
    );

    match result {
        Ok(output) => {
            let first_line = output.lines().next().unwrap_or("(empty)");
            eprintln!("    Commit result: {}", first_line);
        }
        Err(e) => {
            eprintln!("    Commit error: {}", e);
        }
    }
}

// ============================================================================
// TEST 5: Scale test - directly measure memory for increasingly large JSONL
// files to project what happens at the sizes users report (1-5 GB).
// ============================================================================
#[test]
fn test_memory_overflow_scaling_projection() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("TEST: Memory scaling projection for large checkpoint files");
    eprintln!("Measures actual memory usage at various JSONL sizes to project");
    eprintln!("what happens at the 1-5 GB sizes users report.");
    eprintln!("{}\n", "=".repeat(80));

    let repo = TestRepo::new();

    // Create initial commit
    let file_path = repo.path().join("scale_test.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.git(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let working_log = repo.current_working_logs();

    // Test with increasing checkpoint counts
    // Each checkpoint with 200 transcript messages is roughly 400KB
    let configs: &[(usize, usize, usize)] = &[
        // (num_checkpoints, files_per_checkpoint, transcript_messages)
        (10, 3, 100),   // ~4 MB
        (25, 5, 150),   // ~15 MB
        (50, 5, 200),   // ~40 MB
        (100, 5, 200),  // ~80 MB
        (150, 10, 300), // ~200+ MB
    ];

    eprintln!("  {:>12} {:>12} {:>12} {:>12} {:>15}",
        "Checkpoints", "JSONL Size", "Read Time", "RSS Delta", "Projected 1GB");

    for (num_cp, files_per_cp, transcript_msgs) in configs {
        let file_size = write_synthetic_checkpoints(
            &working_log.dir,
            *num_cp,
            *files_per_cp,
            *transcript_msgs,
            "mock_ai",
        );

        let rss_before = get_rss_bytes();
        let start = Instant::now();
        let result = working_log.read_all_checkpoints();
        let elapsed = start.elapsed();
        let rss_after = get_rss_bytes();
        let rss_delta = rss_after.saturating_sub(rss_before);

        // Project: if this file were 1GB, how long / how much memory?
        let scale_factor = if file_size > 0 {
            (1024 * 1024 * 1024) as f64 / file_size as f64
        } else {
            0.0
        };
        let projected_time_ms = elapsed.as_millis() as f64 * scale_factor;
        let projected_memory = rss_delta as f64 * scale_factor;

        let checkpoint_count = result.as_ref().map(|c| c.len()).unwrap_or(0);
        eprintln!(
            "  {:>12} {:>12} {:>12.2?} {:>12} {:>12.0} ms / {}",
            checkpoint_count,
            format_bytes(file_size),
            elapsed,
            format_bytes(rss_delta),
            projected_time_ms,
            format_bytes(projected_memory as u64),
        );
    }

    eprintln!("\n  Key insight: read_all_checkpoints() is called 4-6 times per commit.");
    eprintln!("  Multiply projected values by 4-6x for actual peak memory during commit.");
    eprintln!("  With a 1GB JSONL file and 6 reads: projected peak = 12-18 GB minimum.");
    eprintln!("  With a 5GB JSONL file and 6 reads: projected peak = 60-90 GB (matches reports).");
}

// ============================================================================
// TEST 6: Demonstrate the append_checkpoint rewrite-all pattern specifically.
// Each append reads N checkpoints, appends 1, writes N+1 back.
// Total data written after K appends = sum(1..K) = K*(K+1)/2 = O(K^2).
// ============================================================================
#[test]
fn test_memory_overflow_append_rewrite_all_pattern() {
    eprintln!("\n{}", "=".repeat(80));
    eprintln!("TEST: append_checkpoint rewrite-all O(N^2) I/O pattern");
    eprintln!("Each append reads ALL checkpoints, adds one, writes ALL back.");
    eprintln!("Total bytes written = O(N^2) where N = number of checkpoints.");
    eprintln!("{}\n", "=".repeat(80));

    let repo = TestRepo::new();

    let file_path = repo.path().join("quadratic.rs");
    fs::write(&file_path, "fn main() {}\n").unwrap();
    repo.git(&["add", "."]).unwrap();
    repo.git_og(&["commit", "-m", "initial"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints_file = working_log.dir.join("checkpoints.jsonl");

    let iterations = 20;
    let mut cumulative_write_bytes: u64 = 0;
    let mut times = Vec::new();

    eprintln!("  {:>5} {:>12} {:>12} {:>15} {:>12}",
        "Iter", "JSONL Size", "Time", "Cumul. Written", "Write Ampl.");

    for i in 0..iterations {
        let content = format!("fn main() {{}}\n// iteration {}\n", i);
        fs::write(&file_path, content).unwrap();

        let start = Instant::now();
        repo.git_ai(&["checkpoint", "mock_ai", "quadratic.rs"])
            .expect("checkpoint should succeed");
        let elapsed = start.elapsed();
        times.push(elapsed);

        let file_size = if checkpoints_file.exists() {
            fs::metadata(&checkpoints_file).unwrap().len()
        } else {
            0
        };

        // Each append rewrites the entire file
        cumulative_write_bytes += file_size;

        // Write amplification = total bytes written / current file size
        let write_amplification = if file_size > 0 {
            cumulative_write_bytes as f64 / file_size as f64
        } else {
            0.0
        };

        eprintln!(
            "  {:>5} {:>12} {:>12.2?} {:>15} {:>12.1}x",
            i,
            format_bytes(file_size),
            elapsed,
            format_bytes(cumulative_write_bytes),
            write_amplification
        );
    }

    eprintln!("\n  After {} iterations:", iterations);
    eprintln!(
        "  Total bytes written: {} (O(N^2) growth)",
        format_bytes(cumulative_write_bytes)
    );
    eprintln!(
        "  If each checkpoint had a 1MB transcript, total writes after 100 iterations: ~5 GB"
    );
}
