use super::{extract_address::*, r#impl::*};
use crate::{MsgBuf, RecvResult};
use libc::{sockaddr_storage, socklen_t};
use std::{
    io,
    mem::zeroed,
    net::SocketAddr as InetAddr,
    os::{fd::BorrowedFd, unix::net::SocketAddr as UnixAddr},
};

fn prepare_storage() -> (sockaddr_storage, socklen_t) {
    (unsafe { zeroed() }, 0)
}

fn extract_and_write<T>(abuf: Option<&mut T>, f: impl FnOnce() -> io::Result<T>) -> io::Result<()> {
    if let Some(abuf) = abuf {
        *abuf = f()?;
    }
    Ok(())
}
fn extract_and_write_ip(
    abuf: Option<&mut InetAddr>,
    (storage, len): &(sockaddr_storage, socklen_t),
) -> io::Result<()> {
    extract_and_write(abuf, || extract_ip_address(storage, *len))
}
fn extract_and_write_unix(
    abuf: Option<&mut UnixAddr>,
    (storage, len): &(sockaddr_storage, socklen_t),
) -> io::Result<()> {
    extract_and_write(abuf, || extract_unix_address(storage, *len))
}

pub(crate) fn recv_trunc_ip(
    socket: BorrowedFd<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut InetAddr>,
) -> io::Result<Option<bool>> {
    let mut fused_abuf = prepare_storage();
    let ret = recv_trunc(socket, peek, buf, abuf.is_some().then_some(&mut fused_abuf))?;
    extract_and_write_ip(abuf, &fused_abuf)?;
    Ok(ret)
}
pub(crate) fn recv_trunc_unix(
    socket: BorrowedFd<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut UnixAddr>,
) -> io::Result<Option<bool>> {
    let mut fused_abuf = prepare_storage();
    let ret = recv_trunc(socket, peek, buf, abuf.is_some().then_some(&mut fused_abuf))?;
    extract_and_write_unix(abuf, &fused_abuf)?;
    Ok(ret)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn recv_trunc_with_full_size_ip(
    socket: BorrowedFd<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut InetAddr>,
) -> io::Result<crate::TryRecvResult> {
    let mut fused_abuf = prepare_storage();
    let ret =
        recv_trunc_with_full_size(socket, peek, buf, abuf.is_some().then_some(&mut fused_abuf))?;
    extract_and_write_ip(abuf, &fused_abuf)?;
    Ok(ret)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn recv_trunc_with_full_size_unix(
    socket: BorrowedFd<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut UnixAddr>,
) -> io::Result<crate::TryRecvResult> {
    let mut fused_abuf = prepare_storage();
    let ret =
        recv_trunc_with_full_size(socket, peek, buf, abuf.is_some().then_some(&mut fused_abuf))?;
    extract_and_write_unix(abuf, &fused_abuf)?;
    Ok(ret)
}

pub(crate) fn recv_msg_ip(
    socket: BorrowedFd<'_>,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut InetAddr>,
) -> io::Result<RecvResult> {
    let mut fused_abuf = prepare_storage();
    let ret = recv_msg(socket, buf, abuf.is_some().then_some(&mut fused_abuf))?;
    extract_and_write_ip(abuf, &fused_abuf)?;
    Ok(ret)
}

pub(crate) fn recv_msg_unix(
    socket: BorrowedFd<'_>,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut UnixAddr>,
) -> io::Result<RecvResult> {
    let mut fused_abuf = prepare_storage();
    let ret = recv_msg(socket, buf, abuf.is_some().then_some(&mut fused_abuf))?;
    extract_and_write_unix(abuf, &fused_abuf)?;
    Ok(ret)
}
