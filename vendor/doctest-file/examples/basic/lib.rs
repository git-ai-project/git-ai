/// Adds 1 to the given number, panicking in case of overflow if overflow checks are enabled.
///
/// # Examples
/// ```
#[doc = doctest_file::include_doctest!("examples/basic/doc.rs")]
/// ```
pub fn plus_1(n: u32) -> u32 { n + 1 }
