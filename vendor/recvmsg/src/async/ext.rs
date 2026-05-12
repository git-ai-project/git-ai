use super::*;

/// Futures for reception from socket-like connections with message boundaries with truncation
/// detection.
pub trait TruncatingRecvMsgExt: TruncatingRecvMsg {
    /// Receives one message into the given buffer, returning:
    /// - `Ok(Some(true))` if the message was successfully received;
    /// - `Ok(Some(false))` if it was truncated due to insufficient buffer size;
    /// - `Ok(None)` to indicate end of communication ("EOF");
    /// - `Err(..)` if an I/O error occured.
    ///
    /// If `peek` is `true`, the message is not taken off the queue, meaning that a subsequent call
    /// will return the same message, with bigger buffer sizes receiving more of the message if it
    /// was truncated.
    ///
    /// See [`TruncatingRecvMsg::poll_recv_trunc()`] for implementation contract notes.
    fn recv_trunc<'io, 'buf, 'slice, 'abuf>(
        &'io mut self,
        peek: bool,
        buf: &'buf mut MsgBuf<'slice>,
        abuf: Option<&'abuf mut Self::AddrBuf>,
    ) -> RecvTrunc<'io, 'buf, 'slice, 'abuf, Self>
    where
        Self: Unpin,
    {
        RecvTrunc { recver: self, peek, buf, abuf }
    }

    /// Discards the message at the front of the queue. If at end-of-communication, succeeds with no
    /// effect.
    fn discard_msg(&mut self) -> DiscardMsg<'_, Self>
    where
        Self: Unpin,
    {
        DiscardMsg { recver: self }
    }
}
impl<T: TruncatingRecvMsg + ?Sized> TruncatingRecvMsgExt for T {}

/// Like [`TruncatingRecvMsgExt`], but reports the exact true size of truncated messages.
pub trait TruncatingRecvMsgWithFullSizeExt: TruncatingRecvMsgWithFullSize {
    /// Like [`.recv_trunc()`](TruncatingRecvMsgExt::recv_trunc), but returns the true length
    /// of the message *(size before truncation)*.
    fn recv_trunc_with_full_size<'io, 'buf, 'slice, 'abuf>(
        &'io mut self,
        peek: bool,
        buf: &'buf mut MsgBuf<'slice>,
        abuf: Option<&'abuf mut Self::AddrBuf>,
    ) -> RecvTruncWithFullSize<'io, 'buf, 'slice, 'abuf, Self>
    where
        Self: Unpin,
    {
        RecvTruncWithFullSize { recver: self, peek, buf, abuf }
    }
    /// Attempts to receive one message using the given buffer. If the message at the front of the
    /// queue does not fit, no (re)allocation is done and the message is neither written to the
    /// buffer nor taken off the underlying queue.
    ///
    /// If the operation could not be completed for external reasons, an error from the outermost
    /// `Result` is returned.
    ///
    /// This method simplifies use of `.recv_trunc_with_full_size()` by keeping `buf` consistent in
    /// error conditions and making the call to `.discard_msg()` implicitly as needed.
    fn try_recv_msg<'io, 'buf, 'slice, 'abuf>(
        &'io mut self,
        buf: &'buf mut MsgBuf<'slice>,
        abuf: Option<&'abuf mut Self::AddrBuf>,
    ) -> TryRecv<'io, 'buf, 'slice, 'abuf, Self> {
        TryRecv::new(self, buf, abuf)
    }
}
impl<T: TruncatingRecvMsgWithFullSize + ?Sized> TruncatingRecvMsgWithFullSizeExt for T {}

/// Futures for asynchronously receiving from socket-like connections with message boundaries
/// reliably, without truncation.
pub trait RecvMsgExt: RecvMsg {
    /// Receives one message using the given buffer, (re)allocating the buffer if necessary.
    ///
    /// If the operation could not be completed for external reasons, an error from the outermost
    /// `Result` is returned.
    #[inline]
    fn recv_msg<'io, 'buf, 'slice: 'buf, 'abuf>(
        &'io mut self,
        buf: &'buf mut MsgBuf<'slice>,
        abuf: Option<&'abuf mut Self::AddrBuf>,
    ) -> Recv<'io, 'buf, 'slice, 'abuf, Self>
    where
        Self: Unpin,
    {
        Recv { recver: self, buf, abuf }
    }
}
impl<T: RecvMsg + ?Sized> RecvMsgExt for T {}
