//! Crate with tests for missing docs.

#![deny(missing_docs)]

use rstest::{fixture, rstest};

/// Root fixture
#[fixture]
pub fn root() -> u32 {
    21
}

/// Injection fixture
#[fixture]
pub fn injection(root: u32) -> u32 {
    2 * root
}

/// Success test
#[rstest]
pub fn success(injection: u32) {
    assert_eq!(42, injection);
}

/// Fail test
#[rstest]
pub fn fail(injection: u32) {
    assert_eq!(41, injection);
}
