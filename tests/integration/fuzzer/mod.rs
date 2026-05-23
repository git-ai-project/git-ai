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
// Fixed-seed standard tests (50 ops, all operation types mixed)
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
// Rewrite-heavy tests (40 ops, 50% rewrite ratio)
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
// Checkpoint-heavy tests (80 ops, 40% stress ratio)
// =============================================================================

#[test]
fn fuzz_checkpoint_heavy_0() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(0, 80));
}

#[test]
fn fuzz_checkpoint_heavy_1() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(1, 80));
}

#[test]
fn fuzz_checkpoint_heavy_2() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(2, 80));
}

#[test]
fn fuzz_checkpoint_heavy_55() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(55, 80));
}

#[test]
fn fuzz_checkpoint_heavy_999() {
    run_fuzzer(FuzzerConfig::checkpoint_heavy(999, 80));
}

// =============================================================================
// Partial staging tests (60% partial stage ratio)
// =============================================================================

#[test]
fn fuzz_partial_stage_0() {
    run_fuzzer(FuzzerConfig::partial_stage_heavy(0, 40));
}

#[test]
fn fuzz_partial_stage_1() {
    run_fuzzer(FuzzerConfig::partial_stage_heavy(1, 40));
}

#[test]
fn fuzz_partial_stage_2() {
    run_fuzzer(FuzzerConfig::partial_stage_heavy(2, 40));
}

#[test]
fn fuzz_partial_stage_42() {
    run_fuzzer(FuzzerConfig::partial_stage_heavy(42, 40));
}

#[test]
fn fuzz_partial_stage_99() {
    run_fuzzer(FuzzerConfig::partial_stage_heavy(99, 40));
}

// =============================================================================
// Destructive-heavy tests (45% destructive ops)
// =============================================================================

#[test]
fn fuzz_destructive_0() {
    run_fuzzer(FuzzerConfig::destructive_heavy(0, 40));
}

#[test]
fn fuzz_destructive_1() {
    run_fuzzer(FuzzerConfig::destructive_heavy(1, 40));
}

#[test]
fn fuzz_destructive_2() {
    run_fuzzer(FuzzerConfig::destructive_heavy(2, 40));
}

#[test]
fn fuzz_destructive_42() {
    run_fuzzer(FuzzerConfig::destructive_heavy(42, 40));
}

#[test]
fn fuzz_destructive_99() {
    run_fuzzer(FuzzerConfig::destructive_heavy(99, 40));
}

// =============================================================================
// File operations tests (45% file ops — renames, deletes, subdirs, concurrent)
// =============================================================================

#[test]
fn fuzz_file_ops_0() {
    run_fuzzer(FuzzerConfig::file_ops_heavy(0, 30));
}

#[test]
fn fuzz_file_ops_1() {
    run_fuzzer(FuzzerConfig::file_ops_heavy(1, 30));
}

#[test]
fn fuzz_file_ops_2() {
    run_fuzzer(FuzzerConfig::file_ops_heavy(2, 30));
}

#[test]
fn fuzz_file_ops_42() {
    run_fuzzer(FuzzerConfig::file_ops_heavy(42, 30));
}

#[test]
fn fuzz_file_ops_99() {
    run_fuzzer(FuzzerConfig::file_ops_heavy(99, 30));
}

// =============================================================================
// Stress tests (55% stress ops — rapid bursts, double commits, alternating amends)
// =============================================================================

#[test]
fn fuzz_stress_0() {
    run_fuzzer(FuzzerConfig::stress_heavy(0, 40));
}

#[test]
fn fuzz_stress_1() {
    run_fuzzer(FuzzerConfig::stress_heavy(1, 40));
}

#[test]
fn fuzz_stress_2() {
    run_fuzzer(FuzzerConfig::stress_heavy(2, 40));
}

#[test]
fn fuzz_stress_42() {
    run_fuzzer(FuzzerConfig::stress_heavy(42, 40));
}

#[test]
fn fuzz_stress_99() {
    run_fuzzer(FuzzerConfig::stress_heavy(99, 40));
}

// =============================================================================
// Chaos tests (equal distribution across ALL operation types — max pathological)
// =============================================================================

#[test]
fn fuzz_chaos_0() {
    run_fuzzer(FuzzerConfig::chaos(0, 60));
}

#[test]
fn fuzz_chaos_1() {
    run_fuzzer(FuzzerConfig::chaos(1, 60));
}

#[test]
fn fuzz_chaos_2() {
    run_fuzzer(FuzzerConfig::chaos(2, 60));
}

#[test]
fn fuzz_chaos_42() {
    run_fuzzer(FuzzerConfig::chaos(42, 60));
}

#[test]
fn fuzz_chaos_99() {
    run_fuzzer(FuzzerConfig::chaos(99, 60));
}

#[test]
fn fuzz_chaos_1337() {
    run_fuzzer(FuzzerConfig::chaos(1337, 60));
}

#[test]
fn fuzz_chaos_31415() {
    run_fuzzer(FuzzerConfig::chaos(31415, 60));
}

#[test]
fn fuzz_chaos_65535() {
    run_fuzzer(FuzzerConfig::chaos(65535, 60));
}

#[test]
fn fuzz_chaos_random() {
    let seed: u64 = rand::random_range(0..u64::MAX);
    eprintln!(
        "[fuzzer] CHAOS RANDOM SEED: {} — use this to reproduce failures",
        seed
    );
    run_fuzzer(FuzzerConfig::chaos(seed, 80));
}

// =============================================================================
// Combined operations tests (cherry-pick, branch merge, multi-squash, etc.)
// =============================================================================

#[test]
fn fuzz_combined_0() {
    run_fuzzer(FuzzerConfig::combined_heavy(0, 40));
}

#[test]
fn fuzz_combined_1() {
    run_fuzzer(FuzzerConfig::combined_heavy(1, 40));
}

#[test]
fn fuzz_combined_2() {
    run_fuzzer(FuzzerConfig::combined_heavy(2, 40));
}

#[test]
fn fuzz_combined_42() {
    run_fuzzer(FuzzerConfig::combined_heavy(42, 40));
}

#[test]
fn fuzz_combined_99() {
    run_fuzzer(FuzzerConfig::combined_heavy(99, 40));
}

#[test]
fn fuzz_combined_1337() {
    run_fuzzer(FuzzerConfig::combined_heavy(1337, 40));
}

#[test]
fn fuzz_combined_random() {
    let seed: u64 = rand::random_range(0..u64::MAX);
    eprintln!(
        "[fuzzer] COMBINED RANDOM SEED: {} — use this to reproduce failures",
        seed
    );
    run_fuzzer(FuzzerConfig::combined_heavy(seed, 60));
}

// =============================================================================
// Squash-heavy tests (55% combined ratio — targets squash attribution holes)
// =============================================================================

#[test]
fn fuzz_squash_0() {
    run_fuzzer(FuzzerConfig::squash_heavy(0, 40));
}

#[test]
fn fuzz_squash_1() {
    run_fuzzer(FuzzerConfig::squash_heavy(1, 40));
}

#[test]
fn fuzz_squash_2() {
    run_fuzzer(FuzzerConfig::squash_heavy(2, 40));
}

#[test]
fn fuzz_squash_42() {
    run_fuzzer(FuzzerConfig::squash_heavy(42, 40));
}

#[test]
fn fuzz_squash_99() {
    run_fuzzer(FuzzerConfig::squash_heavy(99, 40));
}

#[test]
fn fuzz_squash_1337() {
    run_fuzzer(FuzzerConfig::squash_heavy(1337, 40));
}

#[test]
fn fuzz_squash_random() {
    let seed: u64 = rand::random_range(0..u64::MAX);
    eprintln!(
        "[fuzzer] SQUASH RANDOM SEED: {} — use this to reproduce failures",
        seed
    );
    run_fuzzer(FuzzerConfig::squash_heavy(seed, 60));
}

// =============================================================================
// Marathon tests (150+ ops, maximum pathological coverage)
// =============================================================================

#[test]
#[ignore]
fn fuzz_marathon_0() {
    run_fuzzer(FuzzerConfig::chaos(0, 150));
}

#[test]
#[ignore]
fn fuzz_marathon_42() {
    run_fuzzer(FuzzerConfig::chaos(42, 150));
}

#[test]
#[ignore]
fn fuzz_marathon_1337() {
    run_fuzzer(FuzzerConfig::chaos(1337, 200));
}

#[test]
#[ignore]
fn fuzz_marathon_random() {
    let seed: u64 = rand::random_range(0..u64::MAX);
    eprintln!(
        "[fuzzer] MARATHON RANDOM SEED: {} — use this to reproduce failures",
        seed
    );
    run_fuzzer(FuzzerConfig::chaos(seed, 200));
}
