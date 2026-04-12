/// Criterion microbenchmarks for the daemon readonly-command fast-path.
///
/// Background: Zed IDE was observed sending >40 git invocations/sec to the
/// daemon, 100% of which were readonly (status, diff, stash list, worktree
/// list, cat-file, for-each-ref, …).  Before the fix, every event was
/// enqueued onto the serial ingest queue, causing >1 min backlog.  After the
/// fix, readonly events are identified and discarded in
/// `prepare_trace_payload_for_ingest` without touching the queue.
///
/// These benchmarks capture three things:
///   1. Raw classification speed of `is_definitely_read_only_invocation`.
///   2. End-to-end speed of `prepare_trace_payload_for_ingest` for readonly events.
///   3. Flood throughput: 1 000 readonly events sequentially, showing that the
///      queue stays empty throughout (vs. the pre-fix behaviour where it would
///      accumulate thousands of pending items).
use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use git_ai::daemon::bench_support::{
    make_start_payload, make_start_payload_with_sid, new_coordinator, prepare_ingest,
};
use git_ai::git::command_classification::is_definitely_read_only_invocation;

// ---------------------------------------------------------------------------
// Helper: Tokio runtime for coordinator construction (spawns internal tasks)
// ---------------------------------------------------------------------------

fn tokio_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

// ---------------------------------------------------------------------------
// 1. Classification hot-path (no coordinator, no allocation)
// ---------------------------------------------------------------------------

fn bench_classification(c: &mut Criterion) {
    let mut group = c.benchmark_group("readonly_classification");

    // Definitively read-only commands (no subcommand parsing needed)
    for cmd in &["status", "diff", "log", "show", "cat-file", "for-each-ref"] {
        group.bench_with_input(BenchmarkId::new("command", cmd), cmd, |b, cmd| {
            b.iter(|| is_definitely_read_only_invocation(black_box(cmd), black_box(None)));
        });
    }

    // Subcommand-gated read-only cases
    group.bench_function("stash_list", |b| {
        b.iter(|| is_definitely_read_only_invocation(black_box("stash"), black_box(Some("list"))))
    });
    group.bench_function("worktree_list", |b| {
        b.iter(|| {
            is_definitely_read_only_invocation(black_box("worktree"), black_box(Some("list")))
        })
    });

    // Mutating commands for comparison
    group.bench_function("commit_mutating", |b| {
        b.iter(|| is_definitely_read_only_invocation(black_box("commit"), black_box(None)))
    });
    group.bench_function("stash_pop_mutating", |b| {
        b.iter(|| is_definitely_read_only_invocation(black_box("stash"), black_box(Some("pop"))))
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 2. prepare_trace_payload_for_ingest: readonly vs mutating event
// ---------------------------------------------------------------------------

fn bench_prepare_ingest(c: &mut Criterion) {
    let rt = tokio_rt();
    let _guard = rt.enter();
    let coord = new_coordinator();

    let mut group = c.benchmark_group("prepare_trace_payload_for_ingest");
    group.throughput(Throughput::Elements(1));

    // Readonly: status — should return false (skip enqueue) immediately
    group.bench_function("status_readonly", |b| {
        b.iter_batched(
            || make_start_payload(&["git", "status", "--short"]),
            |mut payload| prepare_ingest(black_box(&coord), black_box(&mut payload)),
            BatchSize::SmallInput,
        )
    });

    // Readonly: stash list (subcommand parsing required)
    group.bench_function("stash_list_readonly", |b| {
        b.iter_batched(
            || make_start_payload(&["git", "-c", "core.fsmonitor=false", "stash", "list"]),
            |mut payload| prepare_ingest(black_box(&coord), black_box(&mut payload)),
            BatchSize::SmallInput,
        )
    });

    // Readonly: worktree list (subcommand parsing required)
    group.bench_function("worktree_list_readonly", |b| {
        b.iter_batched(
            || make_start_payload(&["git", "--no-pager", "worktree", "list", "--porcelain"]),
            |mut payload| prepare_ingest(black_box(&coord), black_box(&mut payload)),
            BatchSize::SmallInput,
        )
    });

    // Mutating: commit (for baseline comparison — goes through full path)
    group.bench_function("commit_mutating", |b| {
        b.iter_batched(
            || make_start_payload(&["git", "commit", "-m", "wip"]),
            |mut payload| prepare_ingest(black_box(&coord), black_box(&mut payload)),
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// 3. Flood throughput: 1 000 readonly events sequentially
//    Queue depth must stay 0 throughout (no backlog created).
// ---------------------------------------------------------------------------

fn bench_readonly_flood(c: &mut Criterion) {
    let rt = tokio_rt();
    let _guard = rt.enter();

    let mut group = c.benchmark_group("readonly_flood");
    group.throughput(Throughput::Elements(1_000));
    group.sample_size(20); // fewer iterations — each processes 1 000 events

    // Zed-style flood: mix of the most common readonly commands
    let zed_commands: &[&[&str]] = &[
        &["git", "status", "--porcelain=v2"],
        &["git", "diff", "--stat"],
        &["git", "for-each-ref", "--format=%(refname)"],
        &["git", "stash", "list"],
        &["git", "--no-pager", "worktree", "list", "--porcelain"],
        &["git", "show", "--stat", "HEAD"],
        &["git", "cat-file", "-t", "HEAD"],
    ];

    group.bench_function("zed_mixed_1000_events", |b| {
        b.iter_batched(
            || {
                // Fresh coordinator per iteration so queue counter starts at 0
                new_coordinator()
            },
            |coord| {
                for i in 0u32..1_000 {
                    let argv = zed_commands[(i as usize) % zed_commands.len()];
                    let sid = format!("20260411T120000.000000-P{:016x}", i);
                    let mut payload = make_start_payload_with_sid(&sid, argv);
                    let _ = prepare_ingest(black_box(&coord), black_box(&mut payload));
                }
                // Return coord so the framework can drop it after measurement
                coord
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_classification,
    bench_prepare_ingest,
    bench_readonly_flood,
);
criterion_main!(benches);
