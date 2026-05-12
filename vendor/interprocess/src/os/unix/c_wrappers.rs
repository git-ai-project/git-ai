use {
    super::unixprelude::*,
    crate::{os::unix::ud_addr::TerminatedUdAddr, timeout_expiry},
    libc::AF_UNIX,
    std::{
        ffi::CStr,
        io,
        mem::{size_of, MaybeUninit},
        time::{Duration, Instant},
    },
};
#[allow(unused_imports)]
use {
    crate::{FdOrErrno, OrErrno},
    std::ptr,
};

macro_rules! cfg_atomic_cloexec {
    ($($code:tt)+) => {
        #[cfg(any(
            // List taken from the standard library (std/src/sys/net/connection/socket/unix.rs)
            target_os = "android",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "illumos",
            target_os = "hurd",
            target_os = "linux",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "cygwin",
            target_os = "nto",
            target_os = "solaris",
        ))]
        $($code)+
    };
}
macro_rules! cfg_no_atomic_cloexec {
    ($($code:tt)+) => {
        #[cfg(not(any(
            target_os = "android",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "illumos",
            target_os = "hurd",
            target_os = "linux",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "cygwin",
            target_os = "nto",
            target_os = "solaris",
        )))]
        $($code)+
    };
}

pub(super) unsafe fn fcntl_int(fd: BorrowedFd<'_>, cmd: c_int, val: c_int) -> io::Result<c_int> {
    unsafe { libc::fcntl(fd.as_raw_fd(), cmd, val) }.fd_or_errno()
}

fn get_flflags(fd: BorrowedFd<'_>) -> io::Result<c_int> {
    unsafe { fcntl_int(fd, libc::F_GETFL, 0) }
}
fn set_flflags(fd: BorrowedFd<'_>, flags: c_int) -> io::Result<()> {
    unsafe { fcntl_int(fd, libc::F_SETFL, flags) }.map(drop)
}

pub(super) fn set_nonblocking(fd: BorrowedFd<'_>, nonblocking: bool) -> io::Result<()> {
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let old_flags = get_flflags(fd)? & !libc::O_NONBLOCK;
        set_flflags(fd, old_flags | if nonblocking { libc::O_NONBLOCK } else { 0 })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let nonblocking = c_int::from(nonblocking);
        unsafe { libc::ioctl(fd.as_raw_fd(), libc::FIONBIO, ptr::addr_of!(nonblocking)) >= 0 }
            .true_val_or_errno(())
    }
}

/// Like [`set_nonblocking`], but assumes the file descriptor has not been exposed to anyone who
/// could've used `fcntl(F_SETFL)` on it.
pub(super) fn fast_set_nonblocking(fd: BorrowedFd<'_>, nonblocking: bool) -> io::Result<()> {
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        set_flflags(fd, if nonblocking { libc::O_NONBLOCK } else { 0 })
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        set_nonblocking(fd, nonblocking)
    }
}

pub(super) unsafe fn getsockopt<T>(
    fd: BorrowedFd<'_>,
    level: c_int,
    optname: c_int,
) -> io::Result<T> {
    let mut rslt = MaybeUninit::<T>::uninit();
    #[allow(clippy::cast_possible_truncation)] // safety contract
    let orig_len = size_of::<T>() as socklen_t;
    let mut len = orig_len;
    let success = unsafe {
        libc::getsockopt(fd.as_raw_fd(), level, optname, rslt.as_mut_ptr().cast(), &mut len) >= 0
    };
    if len < orig_len {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    success.true_or_errno(|| unsafe { rslt.assume_init() })
}

pub(super) fn duplicate_fd(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    cfg_atomic_cloexec! {{
        let new_fd = unsafe { fcntl_int(fd, libc::F_DUPFD_CLOEXEC, 0)? };
        Ok(unsafe { OwnedFd::from_raw_fd(new_fd) })
    }}
    cfg_no_atomic_cloexec! {{
        let new_fd = unsafe {
            libc::dup(fd.as_raw_fd())
                .fd_or_errno()
                .map(|fd| OwnedFd::from_raw_fd(fd))?
        };
        set_cloexec(new_fd.as_fd())?;
        Ok(new_fd)
    }}
}

pub(super) fn unlink(path: &CStr) -> io::Result<()> {
    unsafe { libc::unlink(path.as_ptr()) != -1 }.true_val_or_errno(())
}

cfg_no_atomic_cloexec! {
    fn set_cloexec(fd: BorrowedFd<'_>) -> io::Result<()> {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) != -1 }.true_val_or_errno(())
    }
}

pub(super) fn set_socket_mode(fd: BorrowedFd<'_>, mode: mode_t) -> io::Result<()> {
    let rslt = unsafe { libc::fchmod(fd.as_raw_fd(), mode) != -1 }.true_val_or_errno(());
    if let Err(e) = &rslt {
        if e.kind() == io::ErrorKind::InvalidInput {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
    }
    rslt
}

const NONBLOCKING_PARAMS: (bool, c_int) = {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
    ))]
    {
        (true, libc::SOCK_NONBLOCK)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "openbsd",
    )))]
    {
        (false, 0)
    }
};
const CAN_CREATE_NONBLOCKING: bool = NONBLOCKING_PARAMS.0;
const NONBLOCKING_FLAG: c_int = NONBLOCKING_PARAMS.1;

/// Creates a Unix domain socket of the given type. If `nonblocking` and
/// [`CAN_CREATE_NONBLOCKING`] are both `true`, also makes it nonblocking.
#[allow(unused_mut)]
fn create_socket(ty: c_int, nonblocking: bool) -> io::Result<OwnedFd> {
    let mut flags = if nonblocking { NONBLOCKING_FLAG } else { 0 };
    cfg_atomic_cloexec! {{
        flags |= libc::SOCK_CLOEXEC;
    }}
    let fd = unsafe { libc::socket(AF_UNIX, ty | flags, 0) }
        .fd_or_errno()
        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })?;
    cfg_no_atomic_cloexec! {{
        set_cloexec(fd.as_fd())?;
    }}

    if !CAN_CREATE_NONBLOCKING && nonblocking {
        set_nonblocking(fd.as_fd(), true)?;
    }

    Ok(fd)
}

fn bind(fd: BorrowedFd<'_>, addr: TerminatedUdAddr<'_>) -> io::Result<()> {
    unsafe { libc::bind(fd.as_raw_fd(), addr.addr_ptr().cast(), addr.addrlen()) != -1 }
        .true_val_or_errno(())
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::EEXIST) {
                io::Error::from_raw_os_error(libc::EADDRINUSE)
            } else {
                e
            }
        })
}

fn listen(fd: BorrowedFd<'_>) -> io::Result<()> {
    // The standard library does this
    #[cfg(any(
        target_os = "windows",
        target_os = "redox",
        target_os = "espidf",
        target_os = "horizon"
    ))]
    const BACKLOG: c_int = 128;
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "macos"
    ))]
    const BACKLOG: c_int = -1;
    #[cfg(not(any(
        target_os = "windows",
        target_os = "redox",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "espidf",
        target_os = "horizon"
    )))]
    const BACKLOG: c_int = libc::SOMAXCONN;
    unsafe { libc::listen(fd.as_raw_fd(), BACKLOG) != -1 }.true_val_or_errno(())
}

pub(super) fn create_listener(
    ty: c_int,
    addr: TerminatedUdAddr<'_>,
    nonblocking: bool,
    mode: Option<mode_t>,
) -> io::Result<OwnedFd> {
    let sock = create_socket(ty, nonblocking)?;
    if let Some(mode) = mode {
        // This used to forbid modes with the executable bit set, but no longer does. That is the
        // OS's business, not ours.
        set_socket_mode(sock.as_fd(), mode)?;
    }
    bind(sock.as_fd(), addr)?;
    listen(sock.as_fd())?;
    if !CAN_CREATE_NONBLOCKING && nonblocking {
        set_nonblocking(sock.as_fd(), true)?;
    }
    Ok(sock)
}

pub(super) fn connect(fd: BorrowedFd<'_>, addr: TerminatedUdAddr<'_>) -> io::Result<()> {
    unsafe { libc::connect(fd.as_raw_fd(), addr.addr_ptr().cast(), addr.addrlen()) != -1 }
        .true_val_or_errno(())
}

/// Creates a client stream by connecting to the given address. `nonblocking` specifies whether
/// the connection is to happen in a nonblocking manner or not. The resulting stream will not
/// have its nonblocking status changed after that.
pub(super) fn create_client(
    dst: TerminatedUdAddr<'_>,
    nonblocking: bool,
) -> io::Result<(OwnedFd, bool)> {
    let sock = create_socket(libc::SOCK_STREAM, nonblocking)?;
    if !CAN_CREATE_NONBLOCKING && nonblocking {
        set_nonblocking(sock.as_fd(), true)?;
    }
    let inprog = match connect(sock.as_fd(), dst) {
        Ok(()) => false,
        Err(e) if matches!(e.raw_os_error(), Some(libc::EINPROGRESS) | Some(libc::EAGAIN)) => {
            true
        }
        Err(e) => return Err(e),
    };
    Ok((sock, inprog))
}

pub(super) fn take_error(fd: BorrowedFd<'_>) -> io::Result<Option<io::Error>> {
    let errno = unsafe { getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR)? };
    Ok((errno != 0).then(|| io::Error::from_raw_os_error(errno)))
}

pub(super) fn wait_for_connect(
    fd: BorrowedFd<'_>,
    timeout: Option<Duration>,
    timeout_msg: &str,
) -> io::Result<()> {
    let revents = poll_loop(fd, libc::POLLOUT, timeout)?;
    // We have to assume there might be an error on VxWorks because it does not
    // set POLLHUP and POLLERR
    if cfg!(target_os = "vxworks") || (revents & (libc::POLLHUP | libc::POLLERR)) != 0 {
        if let Some(e) = take_error(fd)? {
            return Err(e);
        }
    }
    if revents & libc::POLLOUT == 0 {
        return Err(io::Error::new(io::ErrorKind::TimedOut, timeout_msg));
    }
    Ok(())
}

/// Like [`poll`], but loops until one of the events of interest, or an error/hangup, is signaled.
pub(super) fn poll_loop(
    fd: BorrowedFd<'_>,
    events: c_short,
    mut timeout: Option<Duration>,
) -> io::Result<c_short> {
    let end = timeout.map(timeout_expiry).transpose()?;
    loop {
        let rslt = poll(fd, events, timeout)?;
        if rslt & (events | libc::POLLHUP | libc::POLLERR) != 0 {
            break Ok(rslt);
        }
        if let Some(end) = end {
            let remain = end.saturating_duration_since(Instant::now());
            if remain == Duration::ZERO {
                break Ok(0);
            }
            timeout = Some(remain);
        }
        std::hint::spin_loop();
    }
}

pub(super) fn poll(
    fd: BorrowedFd<'_>,
    events: c_short,
    timeout: Option<Duration>,
) -> io::Result<c_short> {
    // NetBSD pollts is identical to ppoll, but named differently for historical
    // reasons. Recent NetBSD versions provide an alias named ppoll to ease
    // porting of Linux programs, but since I bothered to look at the source
    // code, we have no need to depend on that. Hilariously, NetBSD's manpage
    // makes no effort to make it clear that ppoll does not do anything other
    // than call pollts with the exact same arguments it receives. The moral
    // of the story is that you can't do anything on the Berzerklies without
    // reading the source code of the operating system.
    #[cfg(target_os = "netbsd")]
    use libc::pollts as ppoll;
    // https://github.com/rust-lang/libc/pull/4957
    #[cfg(target_os = "openbsd")]
    extern "C" {
        fn ppoll(
            fds: *mut libc::pollfd,
            nfds: libc::nfds_t,
            timeout: *const libc::timespec,
            sigmask: *const libc::sigset_t,
        ) -> c_int;
    }
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly",
    ))]
    use libc::ppoll;

    let mut fd = libc::pollfd { fd: fd.as_raw_fd(), events, revents: 0 };

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "freebsd",
        target_os = "dragonfly",
    ))]
    let scret = unsafe {
        let timeout = timeout.map(duration_to_timespec).transpose()?;
        ppoll(
            &mut fd,
            1,
            timeout.as_ref().map(crate::ref2ptr).unwrap_or(ptr::null()),
            ptr::null(),
        )
    };

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "freebsd",
        target_os = "dragonfly",
    )))]
    let scret = unsafe {
        let timeout =
            timeout.map(|t| c_int::try_from(t.as_millis()).unwrap_or(c_int::MAX)).unwrap_or(-1);
        libc::poll(&mut fd, 1, timeout)
    };

    let ret = (scret >= 0).true_val_or_errno(fd.revents);
    if ret.as_ref().err().and_then(io::Error::raw_os_error) == Some(libc::EINTR) {
        return Ok(0);
    }
    ret
}

#[allow(dead_code)]
pub(super) fn shutdown(fd: BorrowedFd<'_>, how: std::net::Shutdown) -> io::Result<()> {
    use std::net::Shutdown::*;
    let how = match how {
        Read => libc::SHUT_RD,
        Write => libc::SHUT_WR,
        Both => libc::SHUT_RDWR,
    };
    unsafe { libc::shutdown(fd.as_raw_fd(), how) != -1 }.true_val_or_errno(())
}

#[allow(dead_code)]
fn duration_to_timespec(d: Duration) -> io::Result<libc::timespec> {
    let tv_sec = libc::time_t::try_from(d.as_secs()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "timeout duration overflowed time_t")
    })?;
    Ok(libc::timespec { tv_sec, tv_nsec: d.subsec_nanos() as _ })
}
