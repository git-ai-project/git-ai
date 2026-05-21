#[allow(dead_code)]
mod engine;
#[allow(dead_code)]
mod generators;
#[allow(dead_code)]
mod operations;
#[allow(dead_code)]
mod oracle;

use engine::{FuzzerConfig, run_fuzzer};

// =============================================================================
// Fixed-seed tests (50 ops each, fully pathological)
// =============================================================================

#[test]
fn fuzz_seed_0() {
    run_fuzzer(FuzzerConfig::standard(0, 50));
}

#[test]
fn fuzz_seed_1() {
    run_fuzzer(FuzzerConfig::standard(1, 50));
}

#[test]
fn fuzz_seed_2() {
    run_fuzzer(FuzzerConfig::standard(2, 50));
}

#[test]
fn fuzz_seed_3() {
    run_fuzzer(FuzzerConfig::standard(3, 50));
}

#[test]
fn fuzz_seed_4() {
    run_fuzzer(FuzzerConfig::standard(4, 50));
}

#[test]
fn fuzz_seed_5() {
    run_fuzzer(FuzzerConfig::standard(5, 50));
}

#[test]
fn fuzz_seed_6() {
    run_fuzzer(FuzzerConfig::standard(6, 50));
}

#[test]
fn fuzz_seed_7() {
    run_fuzzer(FuzzerConfig::standard(7, 50));
}

#[test]
fn fuzz_seed_8() {
    run_fuzzer(FuzzerConfig::standard(8, 50));
}

#[test]
fn fuzz_seed_9() {
    run_fuzzer(FuzzerConfig::standard(9, 50));
}

// =============================================================================
// Random seed test (100 ops, prints seed on failure for reproduction)
// =============================================================================

#[test]
fn fuzz_random_seed() {
    let seed: u64 = rand::random_range(0..u64::MAX);
    eprintln!(
        "[fuzzer] RANDOM SEED: {} — use this to reproduce failures",
        seed
    );
    run_fuzzer(FuzzerConfig::standard(seed, 100));
}

// =============================================================================
// Rewrite-heavy tests (40 ops, 60% rewrite ratio)
// =============================================================================

#[test]
fn fuzz_rewrite_heavy_42() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(42, 40));
}

#[test]
fn fuzz_rewrite_heavy_99() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(99, 40));
}

#[test]
fn fuzz_rewrite_heavy_777() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(777, 40));
}

#[test]
fn fuzz_rewrite_heavy_1337() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(1337, 40));
}

#[test]
fn fuzz_rewrite_heavy_31415() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(31415, 40));
}

// =============================================================================
// Checkpoint-heavy tests (100 ops, up to 8 edits per commit)
// =============================================================================

#[test]
fn fuzz_checkpoint_heavy_0() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(0, 100));
}

#[test]
fn fuzz_checkpoint_heavy_1() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(1, 100));
}

#[test]
fn fuzz_checkpoint_heavy_2() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(2, 100));
}

#[test]
fn fuzz_checkpoint_heavy_55() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(55, 100));
}

#[test]
fn fuzz_checkpoint_heavy_999() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(999, 100));
}
