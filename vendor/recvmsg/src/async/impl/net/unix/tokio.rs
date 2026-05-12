use crate::{sync::r#impl::net::unix::wrap as syncimpl, MsgBuf, RecvResult};
use std::{
    io,
    net::SocketAddr as InetAddr,
    os::unix::{io::AsFd, net::SocketAddr as UnixAddr},
};
use tokio::net::{self, UdpSocket, UnixDatagram};

fn recv_trunc_ip(
    slf: &mut &UdpSocket,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut InetAddr>,
) -> io::Result<Option<bool>> {
    syncimpl::recv_trunc_ip(slf.as_fd(), peek, buf, abuf)
}
fn recv_trunc_unix(
    slf: &mut &UnixDatagram,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut UnixAddr>,
) -> io::Result<Option<bool>> {
    syncimpl::recv_trunc_unix(slf.as_fd(), peek, buf, abuf)
}
fn recv_msg_ip(
    slf: &mut &UdpSocket,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut InetAddr>,
) -> io::Result<RecvResult> {
    syncimpl::recv_msg_ip(slf.as_fd(), buf, abuf)
}
fn recv_msg_unix(
    slf: &UnixDatagram,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut UnixAddr>,
) -> io::Result<RecvResult> {
    syncimpl::recv_msg_unix(slf.as_fd(), buf, abuf)
}
#[cfg(any(target_os = "linux", target_os = "android"))]
fn recv_trunc_with_full_size_ip(
    slf: &mut &UdpSocket,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut InetAddr>,
) -> io::Result<crate::TryRecvResult> {
    syncimpl::recv_trunc_with_full_size_ip(slf.as_fd(), peek, buf, abuf)
}
#[cfg(any(target_os = "linux", target_os = "android"))]
fn recv_trunc_with_full_size_unix(
    slf: &mut &UnixDatagram,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut UnixAddr>,
) -> io::Result<crate::TryRecvResult> {
    syncimpl::recv_trunc_with_full_size_unix(slf.as_fd(), peek, buf, abuf)
}

impl_atrm!(for net::UdpSocket, with recv_trunc_ip, sa InetAddr);
impl_atrm!(for net::UnixDatagram, with recv_trunc_unix, sa UnixAddr);

impl_arm!(for net::UdpSocket, with recv_msg_ip, sa InetAddr);
impl_arm!(for net::UnixDatagram, with recv_msg_unix, sa UnixAddr);

#[cfg(any(target_os = "linux", target_os = "android"))]
impl_atrmwfs!(for net::UdpSocket, with recv_trunc_with_full_size_ip);
#[cfg(any(target_os = "linux", target_os = "android"))]
impl_atrmwfs!(for net::UnixDatagram, with recv_trunc_with_full_size_unix);
