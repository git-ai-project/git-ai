use crate::{
    AsyncRecvMsg, AsyncTruncatingRecvMsg, AsyncTruncatingRecvMsgWithFullSize, MsgBuf, RecvMsg,
    RecvResult, TruncatingRecvMsg, TruncatingRecvMsgWithFullSize, TryRecvResult,
};
use core::{
    convert::Infallible,
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

// TODO use std type..?

/// Dummy message stream that is at end-of-stream from the outset.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Empty<AddrBuf = ()>(PhantomData<fn(&mut AddrBuf)>);
impl<AddrBuf> Empty<AddrBuf> {
    /// Creates a dummy message stream.
    #[inline(always)]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}
impl<AddrBuf> Default for Empty<AddrBuf> {
    #[inline(always)]
    fn default() -> Self {
        Self::new()
    }
}

impl<AddrBuf> TruncatingRecvMsg for Empty<AddrBuf> {
    type Error = Infallible;
    type AddrBuf = AddrBuf;
    #[inline(always)]
    fn recv_trunc(
        &mut self,
        _: bool,
        _: &mut MsgBuf<'_>,
        _: Option<&mut AddrBuf>,
    ) -> Result<Option<bool>, Self::Error> {
        Ok(None)
    }
}
impl<AddrBuf> TruncatingRecvMsgWithFullSize for Empty<AddrBuf> {
    fn recv_trunc_with_full_size(
        &mut self,
        _: bool,
        _: &mut MsgBuf<'_>,
        _: Option<&mut AddrBuf>,
    ) -> Result<TryRecvResult, Self::Error> {
        Ok(TryRecvResult::EndOfStream)
    }
}
impl<AddrBuf> RecvMsg for Empty<AddrBuf> {
    type Error = Infallible;
    type AddrBuf = AddrBuf;
    #[inline(always)]
    fn recv_msg(
        &mut self,
        _: &mut MsgBuf<'_>,
        _: Option<&mut AddrBuf>,
    ) -> Result<RecvResult, Self::Error> {
        Ok(RecvResult::EndOfStream)
    }
}

impl<AddrBuf> AsyncTruncatingRecvMsg for Empty<AddrBuf> {
    type Error = Infallible;
    type AddrBuf = AddrBuf;
    #[inline(always)]
    fn poll_recv_trunc(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: bool,
        _: &mut MsgBuf<'_>,
        _: Option<&mut AddrBuf>,
    ) -> Poll<Result<Option<bool>, Self::Error>> {
        Ok(None).into()
    }
}
impl<AddrBuf> AsyncTruncatingRecvMsgWithFullSize for Empty<AddrBuf> {
    #[inline(always)]
    fn poll_recv_trunc_with_full_size(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: bool,
        _: &mut MsgBuf<'_>,
        _: Option<&mut AddrBuf>,
    ) -> Poll<Result<TryRecvResult, Self::Error>> {
        Ok(TryRecvResult::EndOfStream).into()
    }
}
impl<AddrBuf> AsyncRecvMsg for Empty<AddrBuf> {
    type Error = Infallible;
    type AddrBuf = AddrBuf;
    #[inline(always)]
    fn poll_recv_msg(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut MsgBuf<'_>,
        _: Option<&mut AddrBuf>,
    ) -> Poll<Result<RecvResult, Self::Error>> {
        Ok(RecvResult::EndOfStream).into()
    }
}
