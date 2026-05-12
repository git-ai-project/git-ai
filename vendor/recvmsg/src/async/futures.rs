use super::*;
use crate::MsgBuf;
use core::future::Future;

macro_rules! futdoc {
    ($trait:ident :: $mtd:ident $($tt:tt)+) => {
        #[doc = concat!(
            "Future type returned by [`.", stringify!($mtd), "(`](", stringify!($trait), "::", stringify!($mtd), ")."
        )]
        $($tt)+
    };
}

futdoc! { TruncatingRecvMsgExt::recv_trunc
#[derive(Debug)]
pub struct RecvTrunc<'io, 'buf, 'slice, 'abuf, TRM: TruncatingRecvMsg + ?Sized> {
    pub(super) recver: &'io mut TRM,
    pub(super) peek: bool,
    pub(super) buf: &'buf mut MsgBuf<'slice>,
    pub(super) abuf: Option<&'abuf mut TRM::AddrBuf>,
}}
impl<TRM: TruncatingRecvMsg + Unpin + ?Sized> Future for RecvTrunc<'_, '_, '_, '_, TRM> {
    type Output = Result<Option<bool>, TRM::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self { recver, peek, buf, abuf } = self.get_mut();
        Pin::new(&mut **recver).poll_recv_trunc(cx, *peek, buf, abuf.as_deref_mut())
    }
}

futdoc! { TruncatingRecvMsgExt::discard_msg
#[derive(Debug)]
pub struct DiscardMsg<'io, TRM: ?Sized> { pub(super) recver: &'io mut TRM }}
impl<TRM: TruncatingRecvMsg + Unpin + ?Sized> Future for DiscardMsg<'_, TRM> {
    type Output = Result<(), TRM::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self { recver } = self.get_mut();
        Pin::new(&mut **recver).poll_discard_msg(cx)
    }
}

futdoc! { TruncatingRecvMsgWithFullSizeExt::recv_trunc_with_full_size
#[derive(Debug)]
pub struct RecvTruncWithFullSize<'io, 'buf, 'slice, 'abuf, TRMWFS: TruncatingRecvMsgWithFullSize + ?Sized> {
    pub(super) recver: &'io mut TRMWFS,
    pub(super) peek: bool,
    pub(super) buf: &'buf mut MsgBuf<'slice>,
    pub(super) abuf: Option<&'abuf mut TRMWFS::AddrBuf>,
}}
impl<TRMWFS: TruncatingRecvMsgWithFullSize + Unpin + ?Sized> Future
    for RecvTruncWithFullSize<'_, '_, '_, '_, TRMWFS>
{
    type Output = Result<TryRecvResult, TRMWFS::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Self { recver, peek, buf, abuf } = self.get_mut();
        Pin::new(&mut **recver).poll_recv_trunc_with_full_size(cx, *peek, buf, abuf.as_deref_mut())
    }
}

futdoc! { TruncatingRecvMsgWithFullSizeExt::try_recv_msg
#[derive(Debug)]
pub struct TryRecv<'io, 'buf, 'slice, 'abuf, TRMWFS: TruncatingRecvMsg + ?Sized> {
    recver: &'io mut TRMWFS,
    state: TryRecvState<'buf, 'slice, 'abuf, TRMWFS::AddrBuf>,
}}
impl<'io, 'buf, 'slice, 'abuf, TRMWFS: TruncatingRecvMsg + ?Sized>
    TryRecv<'io, 'buf, 'slice, 'abuf, TRMWFS>
{
    pub(super) fn new(
        recver: &'io mut TRMWFS,
        buf: &'buf mut MsgBuf<'slice>,
        abuf: Option<&'abuf mut TRMWFS::AddrBuf>,
    ) -> Self {
        Self { recver, state: TryRecvState::Recving { buf, abuf } }
    }
}

#[derive(Debug)]
enum TryRecvState<'buf, 'slice, 'abuf, AB: ?Sized> {
    Recving { buf: &'buf mut MsgBuf<'slice>, abuf: Option<&'abuf mut AB> },
    Discarding,
    End,
}

impl<TRMWFS: TruncatingRecvMsgWithFullSize + Unpin + ?Sized> Future
    for TryRecv<'_, '_, '_, '_, TRMWFS>
{
    type Output = Result<TryRecvResult, TRMWFS::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let slf = self.get_mut();
        match &mut slf.state {
            TryRecvState::Recving { buf, abuf } => {
                let Poll::Ready(rslt) = Pin::new(&mut *slf.recver).poll_recv_trunc_with_full_size(
                    cx,
                    true,
                    buf,
                    abuf.as_deref_mut(),
                )?
                else {
                    return Poll::Pending;
                };
                match rslt {
                    TryRecvResult::Fit => {
                        slf.state = TryRecvState::Discarding;
                        Pin::new(slf).poll(cx)
                    }
                    TryRecvResult::Spilled(sz) => {
                        buf.set_fill(0);
                        buf.has_msg = false;
                        Poll::Ready(Ok(TryRecvResult::Spilled(sz)))
                    }
                    TryRecvResult::EndOfStream => Poll::Ready(Ok(TryRecvResult::EndOfStream)),
                }
            }
            TryRecvState::Discarding => match Pin::new(&mut *slf.recver).poll_discard_msg(cx) {
                Poll::Ready(r) => {
                    slf.state = TryRecvState::End;
                    Poll::Ready(match r {
                        Ok(()) => Ok(TryRecvResult::Fit),
                        Err(e) => Err(e),
                    })
                }
                Poll::Pending => Poll::Pending,
            },
            TryRecvState::End => panic!("attempt to poll a future which has already completed"),
        }
    }
}

futdoc! { RecvMsgExt::recv_msg
#[derive(Debug)]
pub struct Recv<'io, 'buf, 'slice, 'abuf, RM: RecvMsg + ?Sized> {
    pub(super) recver: &'io mut RM,
    pub(super) buf: &'buf mut MsgBuf<'slice>,
    pub(super) abuf: Option<&'abuf mut RM::AddrBuf>
}}
impl<'buf, RM: RecvMsg + Unpin + ?Sized> Future for Recv<'_, 'buf, '_, '_, RM> {
    type Output = Result<RecvResult, RM::Error>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Recv { recver, buf, abuf } = self.get_mut();
        Pin::new(&mut **recver).poll_recv_msg(cx, buf, abuf.as_deref_mut())
    }
}
