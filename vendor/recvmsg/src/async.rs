//! Async reliable message reception trait and its helpers.

// Epic MSRV failure
macro_rules! ready {
    ($e:expr) => {
        match $e {
            ::core::task::Poll::Ready(r) => r,
            ::core::task::Poll::Pending => return ::core::task::Poll::Pending,
        }
    };
}

mod ext;
mod futures;
mod fwd;
mod via;
pub use {ext::*, futures::*, via::*};

mod r#impl {
    mod net {
        #[macro_use]
        mod common {
            #[macro_use]
            pub(super) mod tokio;
        }

        #[cfg(unix)]
        mod unix {
            #[cfg(feature = "tokio")]
            mod tokio;
        }
        #[cfg(windows)]
        mod windows {
            #[cfg(feature = "tokio")]
            mod tokio;
        }

        #[cfg(all(feature = "tokio", test))]
        mod tests;
    }
}

use crate::{MsgBuf, RecvResult, TryRecvResult};
use core::{
    pin::Pin,
    task::{Context, Poll},
};

#[cfg(feature = "tokio")]
fn ioloop<S, R>(
    slf: &mut S,
    cx: &mut Context<'_>,
    mut op: impl FnMut(&mut S) -> std::io::Result<R>,
    mut poll_ready: impl FnMut(&mut S, &mut Context<'_>) -> Poll<std::io::Result<()>>,
) -> Poll<std::io::Result<R>> {
    loop {
        match op(slf) {
            Ok(ok) => break Ok(ok),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                ready!(poll_ready(slf, cx))?;
            }
            Err(e) => break Err(e),
        }
    }
    .into()
}

/// Implementation of reception from socket-like connections with message boundaries with truncation
/// detection.
///
/// This is the async version of [`sync::TruncatingRecvMsg`](super::TruncatingRecvMsg).
pub trait TruncatingRecvMsg {
    /// The I/O error type.
    ///
    /// This exists not only to make error handling around this trait more flexible, but also to
    /// allow the crate to be `#![no_std]`.
    type Error;

    /// The buffer used for sender address reception.
    ///
    /// If sender addresses are not available, this should be [`NoAddrBuf`](crate::NoAddrBuf).
    type AddrBuf;

    /// Polls a future that receives one message into the given buffer, returning within
    /// `Poll::Ready`:
    /// - `Ok(Some(true))` if the message has been successfully received;
    /// - `Ok(Some(false))` if it was truncated due to insufficient buffer size;
    /// - `Ok(None)` to indicate end of communication ("EOF");
    /// - `Err(..)` if an I/O error occured.
    ///
    /// If `peek` is `true`, the message is not taken off the queue, meaning that a subsequent call
    /// will return the same message, with bigger buffer sizes receiving more of the message if it
    /// was truncated.
    ///
    /// In the `Ok(..)` cases, if `abuf` is `Some(..)`, it is filled with the address of the sender.
    ///
    /// # Contract notes
    /// - **Must** set `buf.is_one_msg` to `true` when returning `Poll::Ready(Ok(..))`.
    /// - **Must not** affect the capacity of `buf`.
    /// - **Must not** decrease the initialization cursor or the fill cursor of `buf`.
    /// - **Must** set the fill cursor to the size of the received message (size *after* truncation, not
    ///   actual size of the message) upon returning `Poll::Ready(Ok(..))` and not modify it in any
    ///   other circumstances.
    /// - **Must not** return `Poll::Pending` if the previous poll was `peek = true` and returned
    ///   `Poll::Ready`.
    fn poll_recv_trunc(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Poll<Result<Option<bool>, Self::Error>>;

    /// Polls a future that discards the message at the front of the queue. If at
    /// end-of-communication, succeeds with no effect.
    ///
    /// # Contract notes
    /// - **Must not** return `Poll::Pending` if the previous call to `.poll_recv_trunc()` was
    ///   `peek = true` and returned `Poll::Ready`.
    fn poll_discard_msg(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match ready!(self.poll_recv_trunc(cx, false, &mut MsgBuf::from(&mut [0][..]), None)) {
            Ok(..) => Ok(()),
            Err(e) => Err(e),
        }
        .into()
    }
}
fn _assert_object_safe_trm<E, AB, TRM: TruncatingRecvMsg<Error = E, AddrBuf = AB>>(
    x: &TRM,
) -> &(dyn TruncatingRecvMsg<Error = E, AddrBuf = AB> + '_) {
    x
}

/// Like [`TruncatingRecvMsg`], but reports the exact true size of truncated messages.
///
/// This is the async version of
/// [`sync::TruncatingRecvMsgWithFullSize`](super::TruncatingRecvMsgWithFullSize).
pub trait TruncatingRecvMsgWithFullSize: TruncatingRecvMsg {
    /// Like [`.poll_recv_trunc()`](TruncatingRecvMsg::poll_recv_trunc), but returns the true
    /// length of the message *(size before truncation)*.
    fn poll_recv_trunc_with_full_size(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Poll<Result<TryRecvResult, Self::Error>>;
}
fn _assert_object_safe_trmwfs<
    E,
    AB,
    TRM: TruncatingRecvMsgWithFullSize<Error = E, AddrBuf = AB>,
>(
    x: &TRM,
) -> &(dyn TruncatingRecvMsgWithFullSize<Error = E, AddrBuf = AB> + '_) {
    x
}

/// Implementation of asynchronously receiving from socket-like connections with message boundaries
/// reliably, without truncation.
///
/// This is the async version of [`sync::RecvMsg`](super::RecvMsg).
pub trait RecvMsg {
    /// The I/O error type.
    ///
    /// This exists not only to make error handling around this trait more flexible, but also to
    /// allow the crate to be `#![no_std]`.
    type Error;

    /// The buffer used for sender address reception.
    ///
    /// If sender addresses are not available, this should be [`NoAddrBuf`](crate::NoAddrBuf).
    type AddrBuf;

    /// Polls a future that receives one message using the given buffer, (re)allocating the buffer
    /// if necessary.
    ///
    /// In the `Ok(..)` cases, if `abuf` is `Some(..)`, it is filled with the address of the sender.
    ///
    /// If the operation could not be completed for external reasons, an error from the outermost
    /// `Result` is returned.
    fn poll_recv_msg(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Poll<Result<RecvResult, Self::Error>>;
}
fn _assert_object_safe_rm<E, AB, RM: RecvMsg<Error = E, AddrBuf = AB>>(
    x: &RM,
) -> &(dyn RecvMsg<Error = E, AddrBuf = AB> + '_) {
    x
}
