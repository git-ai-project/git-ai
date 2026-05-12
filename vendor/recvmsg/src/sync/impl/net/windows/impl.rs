use super::extract_address::extract_ip_address;
use crate::{MsgBuf, RecvResult, TruncatingRecvMsg};
use core::mem::{size_of, zeroed, MaybeUninit};
use std::{
    io,
    net::SocketAddr,
    os::windows::io::{AsRawSocket, BorrowedSocket},
};
#[allow(unused_imports)]
use windows_sys::Win32::Networking::WinSock::{
    recv, recvfrom, WSAGetLastError, WSARecvEx, MSG_PARTIAL, MSG_PEEK, SOCKADDR, SOCKADDR_STORAGE,
    SOCKET_ERROR, WSAEMSGSIZE, WSA_ERROR,
};

fn ret_to_result(ret: i32) -> Result<u32, WSA_ERROR> {
    (ret != SOCKET_ERROR).then_some(ret).ok_or_else(|| unsafe { WSAGetLastError() }).map(|i| i as _)
}

fn ptr_and_shortlen(slice: &mut [MaybeUninit<u8>]) -> (*mut u8, i32) {
    (slice.as_mut_ptr().cast(), i32::try_from(slice.len()).unwrap_or(i32::MAX))
}

pub(crate) fn recv_trunc(
    socket: BorrowedSocket<'_>,
    peek: bool,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut SocketAddr>,
) -> io::Result<Option<bool>> {
    buf.set_fill(0);
    buf.has_msg = false;

    let (bptr, blen) = ptr_and_shortlen(&mut buf[..]);
    let mut rabuf = unsafe { zeroed::<SOCKADDR_STORAGE>() };
    let pabuf = ((&mut rabuf) as *mut SOCKADDR_STORAGE).cast::<SOCKADDR>();
    let s = socket.as_raw_socket() as usize;

    let mut flags = 0;
    if peek {
        flags |= MSG_PEEK;
    }
    let mut fromlen = size_of::<SOCKADDR_STORAGE>() as i32;
    let ret = ret_to_result(unsafe {
        if abuf.is_some() {
            recvfrom(s, bptr, blen, flags, pabuf, &mut fromlen)
        } else {
            recv(s, bptr, blen, flags)
        }
    });
    match ret {
        Ok(sz) => {
            let sz = sz as usize;
            unsafe {
                // SAFETY: Windows has initialized that much for us
                buf.advance_init_and_set_fill(sz);
            }
            buf.has_msg = true;
            if let Some(abuf) = abuf {
                *abuf = extract_ip_address(&rabuf, fromlen)?;
            }
            Ok(Some(true))
        }
        Err(WSAEMSGSIZE) => Ok(Some(false)),
        Err(e) => Err(io::Error::from_raw_os_error(e)),
    }
}

#[allow(dead_code)]
fn discard_msg(socket: BorrowedSocket<'_>) -> io::Result<()> {
    recv_trunc(socket, true, &mut MsgBuf::from(&mut [0][..]), None).map(|_| ())
}

// FIXME I've bashed my head against this for a hot minute, and have given up trying to fix it for
// now. The thing that's wrong with this function is WSARecvEx's behavior that I have failed to
// debug thus far: despite returning MSG_PARTIAL, indicating that the rest of the message is to
// follow, it takes the whole message off the queue, acting identically to recv_trunc with
// peek = false. All further calls block indefinitely.

/*
fn recv_piecewise(socket: BorrowedSocket<'_>, buf: &mut MsgBuf<'_>) -> io::Result<RecvResult> {
    let s = socket.as_raw_socket() as usize;
    buf.set_fill(0);
    buf.has_msg = false;
    let mut partial = false;
    let mut spilled = false;
    loop {
        let slice = buf.unfilled_part();
        if slice.is_empty() {
            match buf.grow() {
                Ok(()) => {
                    spilled = true;
                    debug_assert!(!buf.unfilled_part().is_empty());
                    continue;
                }
                Err(e) => {
                    if partial {
                        // A partially successful partial read must result in the rest of the
                        // message being discarded.
                        let _ = discard_msg(socket);
                        return Ok(RecvResult::QuotaExceeded(e));
                    }
                }
            }
            continue;
        }

        let (bptr, blen) = ptr_and_shortlen(slice);
        let mut flags = 0;
        let rslt = ret_to_result(unsafe { WSARecvEx(s, bptr, blen, &mut flags) });

        let incr = match rslt {
            Ok(incr) => incr,
            Err(ec) => {
                let e = io::Error::from_raw_os_error(ec);
                if partial {
                    // This is irrelevant to normal operation of downstream
                    // programs, but still makes them easier to debug.
                    let _ = discard_msg(socket);
                }
                return Err(e);
            }
        };
        unsafe { buf.advance_init_and_set_fill(buf.len_filled() + incr as usize) };

        if (flags as u32 & MSG_PARTIAL != 0) && incr != 0 {
            partial = true;
        } else {
            break;
        }
    }
    Ok(match buf.len_filled() {
        n if spilled => RecvResult::Spilled(n),
        n => RecvResult::Fit(n),
    })
}
*/

pub(crate) fn recv_msg(
    socket: BorrowedSocket,
    buf: &mut MsgBuf<'_>,
    abuf: Option<&mut SocketAddr>,
) -> io::Result<RecvResult> {
    /*
        if let Some(abuf) = abuf {
            match recv_trunc(socket, true, buf, Some(abuf))? {
                None => unreachable!(),
                Some(true) => {
                    discard_msg(socket)?;
                    return Ok(RecvResult::Fit(buf.len_filled()));
                }
                Some(false) => {
                    buf.set_fill(0);
                }
            }
        }
        let ret = recv_piecewise(socket, buf)?;
        debug_assert!(!matches!(ret, RecvResult::Fit(..)));
        Ok(ret)
    */
    struct Impl<'s>(BorrowedSocket<'s>);
    impl TruncatingRecvMsg for Impl<'_> {
        type Error = io::Error;
        type AddrBuf = SocketAddr;
        fn recv_trunc(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut SocketAddr>,
        ) -> io::Result<Option<bool>> {
            recv_trunc(self.0, peek, buf, abuf)
        }
    }
    crate::sync::recv_via_recv_trunc(&mut Impl(socket), buf, abuf)
}
