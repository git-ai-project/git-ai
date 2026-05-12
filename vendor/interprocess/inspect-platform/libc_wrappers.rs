use {
    super::*,
    libc::{mode_t, uid_t},
    std::{
        ffi::CStr,
        io,
        mem::{self, size_of, MaybeUninit},
        os::unix::{ffi::OsStrExt as _, net::UnixListener, prelude::*},
        path::Path,
        ptr::{self, addr_of_mut},
    },
};

trait RetExt: Sized {
    fn is_ok(&self) -> bool;
    fn val_or_errno<T>(self, f: impl FnOnce(Self) -> T) -> io::Result<T> {
        if self.is_ok() {
            Ok(f(self))
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn ok_or_errno(self) -> io::Result<()> { self.val_or_errno(drop) }
}
impl RetExt for c_int {
    fn is_ok(&self) -> bool { *self >= 0 }
}

pub fn geteuid() -> uid_t { unsafe { libc::geteuid() } }
pub fn seteuid(new: uid_t) -> io::Result<()> { unsafe { libc::seteuid(new) }.ok_or_errno() }

pub fn access(path: &CStr, read: bool, write: bool, execute: bool) -> io::Result<()> {
    let mut mask = 0;
    let mut addbit = |cond, bit| {
        if cond {
            mask |= bit;
        }
    };
    addbit(read, libc::R_OK);
    addbit(write, libc::W_OK);
    addbit(execute, libc::X_OK);
    if mask == 0 {
        mask = libc::F_OK;
    }
    unsafe { libc::access(path.as_ptr(), mask) }.ok_or_errno()
}
pub fn stat(path: &CStr) -> io::Result<libc::stat> {
    let mut out = MaybeUninit::uninit();
    unsafe { libc::stat(path.as_ptr(), out.as_mut_ptr()) }
        .val_or_errno(|_| unsafe { out.assume_init() })
}
pub fn fstat(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
    let mut out = MaybeUninit::uninit();
    unsafe { libc::fstat(fd.as_raw_fd(), out.as_mut_ptr()) }
        .val_or_errno(|_| unsafe { out.assume_init() })
}
pub fn fchmod(fd: BorrowedFd<'_>, mode: mode_t) -> io::Result<()> {
    unsafe { libc::fchmod(fd.as_raw_fd(), mode) }.ok_or_errno()
}

#[allow(clippy::cast_possible_truncation)]
fn sockaddr_un_init(
    sau: &mut MaybeUninit<libc::sockaddr_un>,
    path: &Path,
) -> io::Result<libc::socklen_t> {
    const SUN_PATH_LEN: usize = {
        let sau = unsafe { mem::zeroed::<libc::sockaddr_un>() };
        sau.sun_path.len()
    };

    std::os::unix::net::SocketAddr::from_pathname(path)?;

    let sauptr = sau.as_mut_ptr();
    let path_bytes = path.as_os_str().as_bytes();
    let pathlen = path_bytes.len();
    assert!(pathlen <= SUN_PATH_LEN);
    unsafe {
        addr_of_mut!((*sauptr).sun_family).write(libc::AF_UNIX as _);
        let pathbase = addr_of_mut!((*sauptr).sun_path).cast::<c_char>();
        ptr::copy_nonoverlapping(path_bytes.as_ptr().cast(), pathbase, path_bytes.len());
        if path_bytes.len() != SUN_PATH_LEN {
            pathbase.add(pathlen).write(0);
        }
    }
    let addrlen = (size_of::<libc::sockaddr_un>() - SUN_PATH_LEN) + pathlen;
    Ok(addrlen as _)
}

pub fn bind_with_hook(
    path: &Path,
    f: impl FnOnce(BorrowedFd<'_>) -> io::Result<()>,
) -> io::Result<UnixListener> {
    let mut sau = MaybeUninit::<libc::sockaddr_un>::uninit();
    let addrlen = sockaddr_un_init(&mut sau, path)?;

    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) }
        .val_or_errno(|fd| unsafe { OwnedFd::from_raw_fd(fd) })?;
    f(fd.as_fd())?;
    unsafe { libc::bind(fd.as_raw_fd(), sau.as_ptr().cast(), addrlen) }.ok_or_errno()?;
    Ok(fd.into())
}

pub fn umask(mask: mode_t) -> UmaskGuard { UmaskGuard(unsafe { libc::umask(mask) }) }
#[derive(Debug)]
#[repr(transparent)]
pub struct UmaskGuard(mode_t);
impl Drop for UmaskGuard {
    fn drop(&mut self) { mem::forget(umask(self.0)) }
}
