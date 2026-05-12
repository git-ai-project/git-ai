#![doc = include_str!("../README.md")]
#![cfg_attr(feature = "doc_cfg", feature(doc_cfg))]
// If this was in Cargo.toml, it would cover examples as well
#![warn(
    missing_docs,
    clippy::panic_in_result_fn,
    clippy::missing_assert_message,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod platform_check;

// TODO inspect panic points

#[macro_use]
mod macros;

pub mod bound_util;
pub mod error;
pub mod local_socket;
pub mod unnamed_pipe;

/// Platform-specific functionality for various interprocess communication primitives.
///
/// This module houses two modules: `unix` and `windows`, although only one at a time will be
/// visible, depending on which platform the documentation was built on. If you're using
/// [Docs.rs](https://docs.rs/interprocess/latest/interprocess), you can view the documentation for
/// Windows, macOS, Linux and FreeBSD using the Platform menu on the Docs.rs-specific header bar at
/// the top of the page. Docs.rs builds also have the nightly-only `doc_cfg` feature enabled by
/// default, with which everything platform-specific has a badge next to it which specifies the
/// `cfg(...)` conditions for that item to be available.
pub mod os {
    #[cfg(unix)]
    #[cfg_attr(feature = "doc_cfg", doc(cfg(unix)))]
    pub mod unix;
    #[cfg(windows)]
    #[cfg_attr(feature = "doc_cfg", doc(cfg(windows)))]
    pub mod windows;
}

/// Describes how a client connection operation should wait for the server to accept it.
#[derive(Copy, Clone, Debug, Default)]
pub enum ConnectWaitMode {
    /// The connection operation returns immediately. Subsequent I/O operations will block until
    /// the connection actually becomes established. If a connection error occurs in the
    /// background, that error will be returned by the next I/O operation on the returned object.
    Deferred,
    /// A wait state is entered until the connection becomes established which lasts for up to the
    /// given amount of time. An error of kind [`TimedOut`](std::io::ErrorKind::WouldBlock) is
    /// returned if it does not become established within that timeframe.
    Timeout(std::time::Duration),
    /// A wait state is entered until the connection becomes established. This wait state may
    /// last for an indefinite amount of time.
    #[default]
    Unbounded,
}
impl ConnectWaitMode {
    fn timeout_or_unsupported(self, emsg: &str) -> std::io::Result<Option<std::time::Duration>> {
        match self {
            Self::Deferred => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, emsg)),
            Self::Timeout(t) => Ok(Some(t)),
            Self::Unbounded => Ok(None),
        }
    }
}

mod try_clone;
pub use try_clone::*;

mod atomic_enum;
mod misc;
pub(crate) use {atomic_enum::*, misc::*};

#[cfg(test)]
#[path = "../tests/index.rs"]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects, clippy::indexing_slicing)]
mod tests;
