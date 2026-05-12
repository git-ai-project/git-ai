//! This module defines the trait [`Num`].
//!
//! One way to implement it would be
//!  ```
#![doc = doctest_file::include_doctest!("examples/anchors/zero.rs")]
//! ```

mod one_two;
mod zero;

/// Like, how many?
///
/// # Examples
///
/// `Num` can be implemented for several types:
/// ```rust
#[doc = doctest_file::include_doctest!("examples/anchors/one_two.rs", region = "impl")]
/// ```
///
/// For `One`, this means:
///
/// ```rust
#[doc = doctest_file::include_doctest!("examples/anchors/one_two.rs", region = "impl", hidden)]
/// // same setup as above
#[doc = doctest_file::include_doctest!("examples/anchors/one_two.rs", region = "one_is_1")]
/// ```
///
/// However, `Two` is implemented differently:
///
/// ```rust
#[doc = doctest_file::include_doctest!("examples/anchors/one_two.rs", region = "impl", hidden)]
/// // again, same setup
#[doc = doctest_file::include_doctest!("examples/anchors/one_two.rs", region = "impls_differ")]
/// ```
///
/// Of course, given the implementation from the [crate root](crate), this implies:
///
/// ```rust
#[doc = doctest_file::include_doctest!("examples/anchors/one_two.rs", hidden, region = "impl")]
#[doc = doctest_file::include_doctest!("examples/anchors/zero.rs", hidden)]
/// assert!(Zero.num() < One.num());
/// ```
pub trait Num {
    fn num(&self) -> u8;
}
