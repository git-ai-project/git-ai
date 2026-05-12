#![allow(unsafe_code)]

mod extract_address;
mod r#impl;
pub(crate) use r#impl::*;

use crate::{MsgBuf, RecvMsg, RecvResult, TruncatingRecvMsg};
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    os::windows::io::AsSocket,
};

impl TruncatingRecvMsg for &UdpSocket {
    type Error = io::Error;
    type AddrBuf = SocketAddr;
    #[inline]
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut SocketAddr>,
    ) -> io::Result<Option<bool>> {
        recv_trunc(self.as_socket(), peek, buf, abuf)
    }
}

impl TruncatingRecvMsg for UdpSocket {
    type Error = io::Error;
    type AddrBuf = SocketAddr;
    #[inline]
    fn recv_trunc(
        &mut self,
        peek: bool,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut SocketAddr>,
    ) -> io::Result<Option<bool>> {
        (&*self).recv_trunc(peek, buf, abuf)
    }
}

impl RecvMsg for &UdpSocket {
    type Error = io::Error;
    type AddrBuf = SocketAddr;
    #[inline]
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut SocketAddr>,
    ) -> io::Result<RecvResult> {
        recv_msg(self.as_socket(), buf, abuf)
    }
}
impl RecvMsg for UdpSocket {
    type Error = io::Error;
    type AddrBuf = SocketAddr;
    #[inline]
    fn recv_msg(
        &mut self,
        buf: &mut MsgBuf<'_>,
        abuf: Option<&mut SocketAddr>,
    ) -> io::Result<RecvResult> {
        RecvMsg::recv_msg(&mut &*self, buf, abuf)
    }
}
