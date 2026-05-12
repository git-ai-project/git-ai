use super::*;

/// Convenience methods for [`TruncatingRecvMsgWithFullSize`].
pub trait TruncatingRecvMsgWithFullSizeExt: TruncatingRecvMsgWithFullSize {
    /// Attempts to receive one message using the given buffer. If the message at the front of the
    /// queue does not fit, no (re)allocation is done and the message is neither written to the
    /// buffer nor taken off the underlying queue.
    ///
    /// In the `Ok(..)` case, if `abuf` is `Some(..)`, it is filled with the address of the sender.
    ///
    /// If the operation could not be completed for external reasons, an error from the outermost
    /// `Result` is returned.
    ///
    /// This method simplifies use of `.recv_trunc_with_full_size()` by keeping `buf` consistent in
    /// error conditions and making the call to `.discard_msg()` implicitly as needed.
    fn try_recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut Self::AddrBuf>,
    ) -> Result<TryRecvResult, Self::Error> {
        Ok(match self.recv_trunc_with_full_size(true, buf, abuf)? {
            TryRecvResult::Fit => {
                self.discard_msg()?;
                TryRecvResult::Fit
            }
            TryRecvResult::Spilled(sz) => {
                buf.set_fill(0);
                buf.has_msg = false;
                TryRecvResult::Spilled(sz)
            }
            TryRecvResult::EndOfStream => TryRecvResult::EndOfStream,
        })
    }
}
impl<T: TruncatingRecvMsgWithFullSize + ?Sized> TruncatingRecvMsgWithFullSizeExt for T {}
