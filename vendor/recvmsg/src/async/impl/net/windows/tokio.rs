use crate::{sync::r#impl::net::windows as syncimpl, MsgBuf, RecvResult};
use std::{io, net::SocketAddr, os::windows::io::AsSocket};
use tokio::net::UdpSocket;

fn recv_trunc(
    slf: &mut &UdpSocket,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut SocketAddr>,
) -> io::Result<Option<bool>> {
    syncimpl::recv_trunc(slf.as_socket(), peek, buf, abuf)
}
fn recv_msg(
    slf: &mut &UdpSocket,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut SocketAddr>,
) -> io::Result<RecvResult> {
    syncimpl::recv_msg(slf.as_socket(), buf, abuf)
}

impl_atrm!(for UdpSocket, with recv_trunc, sa SocketAddr);
impl_arm!(for UdpSocket, with recv_msg, sa SocketAddr);
