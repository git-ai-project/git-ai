#![allow(unsafe_code)]

mod extract_address;
mod r#impl;
pub(crate) mod wrap;

use crate::{MsgBuf, RecvMsg, RecvResult, TruncatingRecvMsg};
#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::{TruncatingRecvMsgWithFullSize, TryRecvResult};
use std::{
    io,
    net::{SocketAddr as InetAddr, UdpSocket},
    os::{
        fd::AsFd,
        unix::net::{SocketAddr as UnixAddr, UnixDatagram},
    },
};

impl TruncatingRecvMsg for &UdpSocket {
    type Error = io::Error;
    type AddrBuf = InetAddr;
    #[inline]
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut InetAddr>,
    ) -> io::Result<Option<bool>> {
        wrap::recv_trunc_ip(self.as_fd(), peek, buf, abuf)
    }
}

impl TruncatingRecvMsg for UdpSocket {
    type Error = io::Error;
    type AddrBuf = InetAddr;
    #[inline]
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut InetAddr>,
    ) -> io::Result<Option<bool>> {
        (&*self).recv_trunc(peek, buf, abuf)
    }
}

/// Linux-only, requires kernel 3.4 or newer.
#[cfg(any(target_os = "linux", target_os = "android"))]
impl TruncatingRecvMsgWithFullSize for &UdpSocket {
    #[inline]
    fn recv_trunc_with_full_size(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut InetAddr>,
    ) -> io::Result<TryRecvResult> {
        wrap::recv_trunc_with_full_size_ip(self.as_fd(), peek, buf, abuf)
    }
}

/// Linux-only, requires kernel 3.4 or newer.
#[cfg(any(target_os = "linux", target_os = "android"))]
impl TruncatingRecvMsgWithFullSize for UdpSocket {
    #[inline]
    fn recv_trunc_with_full_size(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut InetAddr>,
    ) -> io::Result<TryRecvResult> {
        (&*self).recv_trunc_with_full_size(peek, buf, abuf)
    }
}

impl RecvMsg for &UdpSocket {
    type Error = io::Error;
    type AddrBuf = InetAddr;
    #[inline]
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut InetAddr>,
    ) -> io::Result<RecvResult> {
        wrap::recv_msg_ip(self.as_fd(), buf, abuf)
    }
}

impl RecvMsg for UdpSocket {
    type Error = io::Error;
    type AddrBuf = InetAddr;
    #[inline]
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut InetAddr>,
    ) -> io::Result<RecvResult> {
        (&mut &*self).recv_msg(buf, abuf)
    }
}

impl TruncatingRecvMsg for &UnixDatagram {
    type Error = io::Error;
    type AddrBuf = UnixAddr;
    #[inline]
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut UnixAddr>,
    ) -> io::Result<Option<bool>> {
        wrap::recv_trunc_unix(self.as_fd(), peek, buf, abuf)
    }
}

impl TruncatingRecvMsg for UnixDatagram {
    type Error = io::Error;
    type AddrBuf = UnixAddr;
    #[inline]
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut UnixAddr>,
    ) -> io::Result<Option<bool>> {
        (&*self).recv_trunc(peek, buf, abuf)
    }
}

/// Linux-only, requires kernel 3.4 or newer.
#[cfg(any(target_os = "linux", target_os = "android"))]
impl TruncatingRecvMsgWithFullSize for &UnixDatagram {
    #[inline]
    fn recv_trunc_with_full_size(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut UnixAddr>,
    ) -> io::Result<TryRecvResult> {
        wrap::recv_trunc_with_full_size_unix(self.as_fd(), peek, buf, abuf)
    }
}

/// Linux-only, requires kernel 3.4 or newer.
#[cfg(any(target_os = "linux", target_os = "android"))]
impl TruncatingRecvMsgWithFullSize for UnixDatagram {
    #[inline]
    fn recv_trunc_with_full_size(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut UnixAddr>,
    ) -> io::Result<TryRecvResult> {
        (&*self).recv_trunc_with_full_size(peek, buf, abuf)
    }
}

impl RecvMsg for &UnixDatagram {
    type Error = io::Error;
    type AddrBuf = UnixAddr;
    #[inline]
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut UnixAddr>,
    ) -> io::Result<RecvResult> {
        wrap::recv_msg_unix(self.as_fd(), buf, abuf)
    }
}

impl RecvMsg for UnixDatagram {
    type Error = io::Error;
    type AddrBuf = UnixAddr;
    #[inline]
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut UnixAddr>,
    ) -> io::Result<RecvResult> {
        (&mut &*self).recv_msg(buf, abuf)
    }
}
