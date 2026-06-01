#[allow(dead_code)]
mod engine;
#[allow(dead_code)]
mod helpers;
#[allow(dead_code)]
mod model;
#[allow(dead_code)]
mod operations;

use engine::{FuzzerConfig, run_fuzzer};

#[test]
fn fuzz_standard_seed_0() {
    run_fuzzer(FuzzerConfig::standard(0, 20));
}

#[test]
fn fuzz_standard_seed_1() {
    run_fuzzer(FuzzerConfig::standard(1, 20));
}

#[test]
fn fuzz_standard_seed_42() {
    run_fuzzer(FuzzerConfig::standard(42, 20));
}

#[test]
fn fuzz_standard_seed_99() {
    run_fuzzer(FuzzerConfig::standard(99, 20));
}

#[test]
fn fuzz_standard_seed_1337() {
    run_fuzzer(FuzzerConfig::standard(1337, 20));
}

#[test]
fn fuzz_rewrite_heavy_seed_0() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(0, 20));
}

#[test]
fn fuzz_rewrite_heavy_seed_42() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(42, 20));
}

#[test]
fn fuzz_rewrite_heavy_seed_99() {
    run_fuzzer(FuzzerConfig::rewrite_heavy(99, 20));
}

#[test]
fn fuzz_random() {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    run_fuzzer(FuzzerConfig::standard(seed, 20));
}
