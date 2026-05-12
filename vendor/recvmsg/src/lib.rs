//! [![Rust version: 1.60+](https://img.shields.io/badge/rust%20version-1.60+-orange)](https://blog.rust-lang.org/2021/10/21/Rust-1.60.0.html)
//!
//! Traits for receiving datagrams reliably, without truncation.
//!
//! # Problem
//! Unlike a byte stream interface, datagram sockets (most notably UDP) and other packet-based APIs
//! preserve boundaries between different write calls, which is what "message boundary" essentially
//! means. Extracting messages by partial reads is an error-prone task, which is why no such
//! interface is exposed by any OS – instead, all messages received from message IPC channels are
//! full messages rather than chunks of messages, which simplifies things to a great degree and is
//! arguably the only proper way of implementing datagram support.
//!
//! There is one pecularity related to this design: you can't just use a buffer with arbitrary
//! length to successfully receive a message. With byte streams, that always works – there either is
//! some data which can be written into that buffer or end of file has been reached, aside from the
//! implied error case which is always a possibility for any kind of I/O. With datagrams,
//! however, **there might not always be enough space in a buffer to fetch a whole message**. If the
//! buffer is too small to fetch part of a message, it is truncated and the message ends up
//! essentially malformed.
//!
//! # Solution
//! The [`RecvMsg`] trait (together with its async counterpart, [`AsyncRecvMsg`]) provides an
//! interface that completely prevents truncation.
//!
//! With the help of [`MsgBuf`], a borrowed buffer can be provided, which can also be subsequently
//! transitioned into an owned buffer as needed. Alternatively, [`MsgBuf`] can start off with an
//! already-owned buffer. The inner `Vec` will then be resized as necessary, with an optional quota
//! preventing a connection from exhausting all memory.
//!
//! # Implementation
//! There are three features a standard truncating message reception can provide to allow programs
//! to solve the truncation problem: peeking, truncation reporting and exact length querying. The
//! former two are represented by the [`TruncatingRecvMsg`] trait, while the last one can be seen as
//! an extension of those and is thus available as [`TruncatingRecvMsgWithFullSize`]. Both of those
//! have async counterparts.
//!
//! [`RecvMsg`] or [`AsyncRecvMsg`] are then to be implemented in terms of either of those traits
//! using the appropriate helper function from the corresponding module.
//!
//! # Feature flags
//! - *`std`* – `std::error::Error` on [`QuotaExceeded`]. Precludes `#![no_std]`.
//! - *`std_net`* – implementations of traits on types from `std::net` and `std::os::unix::net`
//!   (Unix domain sockets) on Unix.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs, unsafe_code)]
extern crate alloc;

// TODO vectored
// TODO async-std
// TODO from_fns

#[macro_use]
mod macros;

pub mod r#async; // ya can't stop me
pub mod msgbuf;
pub mod prelude;
pub mod sync;

/// OS-specific functionality, in particular that which has public APIs that go beyond trait
/// implementations.
///
/// Only available when the standard library is enabled. Items from foreign platforms are not
/// visible in Rustdoc.
#[cfg(feature = "std")]
pub mod os {
    /// Unix-specific functionality.
    #[cfg(all(unix, feature = "std_net"))]
    pub mod unix;
}

mod empty;

pub use {empty::*, msgbuf::QuotaExceeded, prelude::*};

#[track_caller]
fn panic_try_recv_retcon() -> ! {
    panic!(
        "\
try_recv_msg() returned TryRecvResult::Failed for a buffer of a size that it reported was \
sufficient"
    )
}

/// The type of `AddrBuf` associated types on implementations of traits from this crate for types
/// that do not support receiving the address of the peer together with received messages.
pub type NoAddrBuf = core::convert::Infallible;

/// Result type for `.recv_msg()` methods.
#[derive(Copy, Clone, Debug, Default)]
pub enum RecvResult {
    /// The message stream has ended and no more messages will be received.
    #[default]
    EndOfStream,
    /// The message successfully fit into the provided buffer and is of the given size.
    Fit,
    /// The message didn't fit into the provided buffer, and has been received into [`MsgBuf`]'s
    /// `owned` field, which has been updated with a new or extended allocation.
    Spilled,
    /// The buffer size quota was exceeded.
    QuotaExceeded(QuotaExceeded),
}
impl From<TryRecvResult> for RecvResult {
    #[inline]
    fn from(rslt: TryRecvResult) -> Self {
        match rslt {
            TryRecvResult::EndOfStream => Self::EndOfStream,
            TryRecvResult::Fit => Self::Fit,
            TryRecvResult::Spilled(_) => Self::Spilled,
        }
    }
}

/// Result type for `.try_recv_msg()` and `.recv_trunc_with_full_size()` methods.
#[derive(Copy, Clone, Debug, Default)]
pub enum TryRecvResult {
    /// The message stream has ended and no more messages will be received.
    #[default]
    EndOfStream,
    /// The message successfully fit into the provided buffer and is of the given size.
    Fit,
    /// The message didn't fit into the provided buffer.
    /// - If returned by `.try_recv_msg()`, this means that initialized part of the buffer has not
    ///   been modified, and the message at the front of the queue is of the given size.
    /// - If returned by `.recv_trunc_with_full_size()`, this means that the message was truncated.
    Spilled(usize),
}
