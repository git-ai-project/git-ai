//! Non-async reliable message reception trait and its helpers.

mod ext;
mod fwd;
mod via;
pub use {ext::*, via::*};

pub(crate) mod r#impl {
    #[cfg(feature = "std_net")]
    pub(crate) mod net {
        #[cfg(unix)]
        pub(crate) mod unix;
        #[cfg(windows)]
        pub(crate) mod windows;

        #[cfg(test)]
        mod tests;
    }
}

use crate::{MsgBuf, RecvResult, TryRecvResult};

/// Receiving from socket-like connections with message boundaries with truncation detection.
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

    /// Receives one message into the given buffer, returning:
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
    /// - **Must** set `buf.is_one_msg` to `true` when returning `Ok(..)`.
    /// - **Must not** affect the capacity of `buf`.
    /// - **Must not** decrease the initialization cursor or the fill cursor of `buf`.
    /// - **Must** set the fill cursor to the size of the received message (size *after* truncation,
    ///   not actual size of the message) and not modify it in any other circumstances.
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Result<Option<bool>, Self::Error>;

    /// Discards the message at the front of the queue. If at end-of-communication, succeeds with no
    /// effect.
    fn discard_msg(&mut self) -> Result<(), Self::Error> {
        self.recv_trunc(false, &mut MsgBuf::from(&mut [0][..]), None)?;
        Ok(())
    }
}
fn _assert_object_safe_trm<E, AB, TRM: TruncatingRecvMsg<Error = E, AddrBuf = AB>>(
    x: &TRM,
) -> &(dyn TruncatingRecvMsg<Error = E, AddrBuf = AB> + '_) {
    x
}

/// Like [`TruncatingRecvMsg`], but reports the exact true size of truncated messages.
pub trait TruncatingRecvMsgWithFullSize: TruncatingRecvMsg {
    /// Like [`.recv_trunc()`](TruncatingRecvMsg::recv_trunc), but returns the true length of the
    /// message *(size before truncation)*.
    fn recv_trunc_with_full_size(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Result<TryRecvResult, Self::Error>;
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

/// Receiving from socket-like connections with message boundaries without truncation.
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

    /// Receives one message using the given buffer, (re)allocating the buffer if necessary.
    ///
    /// In the `Ok(..)` case, if `abuf` is `Some(..)`, it is filled with the address of the sender.
    ///
    /// If the operation could not be completed for external reasons, an error from the outermost
    /// `Result` is returned.
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Result<RecvResult, Self::Error>;
}
fn _assert_object_safe_rm<E, AB, RM: RecvMsg<Error = E, AddrBuf = AB>>(
    x: &RM,
) -> &(dyn RecvMsg<Error = E, AddrBuf = AB> + '_) {
    x
}
