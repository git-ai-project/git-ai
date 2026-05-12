#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::TryRecvResult;
use crate::{os::unix::recv_trunc_recvmsg_with_msghdr, MsgBuf, RecvResult, TruncatingRecvMsg};
use libc::{msghdr, sockaddr_storage, socklen_t, MSG_PEEK};
use std::{io, mem::zeroed, os::fd::BorrowedFd};

pub(crate) fn recv_trunc(
    fd: BorrowedFd<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    mut abuf: Option<&mut (sockaddr_storage, socklen_t)>,
) -> io::Result<Option<bool>> {
    unsafe {
        let mut hdr = zeroed::<msghdr>();
        let ret = recv_trunc_recvmsg_with_msghdr(
            fd,
            &mut hdr,
            buf,
            abuf.as_deref_mut().map(|(s, _)| s),
            if peek { MSG_PEEK } else { 0 },
        )?
        .0;
        if let Some((_, ref mut nl)) = abuf {
            *nl = hdr.msg_namelen;
        }
        Ok(ret)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn recv_trunc_with_full_size(
    fd: BorrowedFd<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    mut abuf: Option<&mut (sockaddr_storage, socklen_t)>,
) -> io::Result<TryRecvResult> {
    Ok(
        match unsafe {
            let mut hdr = zeroed::<msghdr>();
            let rtr = recv_trunc_recvmsg_with_msghdr(
                fd,
                &mut hdr,
                buf,
                abuf.as_deref_mut().map(|(s, _)| s),
                libc::MSG_TRUNC | if peek { MSG_PEEK } else { 0 },
            )?;
            if let Some((_, ref mut nl)) = abuf {
                *nl = hdr.msg_namelen;
            }
            rtr
        } {
            (Some(true), sz) => TryRecvResult::Fit(sz),
            (Some(false), sz) => TryRecvResult::Spilled(sz),
            (None, ..) => TryRecvResult::EndOfStream,
        },
    )
}

#[inline]
pub(crate) fn recv_msg(
    fd: BorrowedFd<'_>,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut (sockaddr_storage, socklen_t)>,
) -> io::Result<RecvResult> {
    struct Impl<'a>(BorrowedFd<'a>);
    impl TruncatingRecvMsg for Impl<'_> {
        type Error = io::Error;
        type AddrBuf = (sockaddr_storage, socklen_t);
        fn recv_trunc(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut (sockaddr_storage, socklen_t)>,
        ) -> Result<Option<bool>, Self::Error> {
            recv_trunc(self.0, peek, buf, abuf)
        }
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    impl crate::TruncatingRecvMsgWithFullSize for Impl<'_> {
        fn recv_trunc_with_full_size(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut (sockaddr_storage, socklen_t)>,
        ) -> Result<TryRecvResult, Self::Error> {
            recv_trunc_with_full_size(self.0, peek, buf, abuf)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        crate::sync::recv_via_try_recv(&mut Impl(fd), buf, abuf)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        crate::sync::recv_via_recv_trunc(&mut Impl(fd), buf, abuf)
    }
}
