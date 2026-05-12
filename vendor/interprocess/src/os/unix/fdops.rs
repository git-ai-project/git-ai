use {
    super::{c_wrappers, unixprelude::*},
    crate::{AsBuf, OrErrno, TryClone},
    std::io::{self, prelude::*, IoSlice, IoSliceMut},
};

#[allow(clippy::cast_sign_loss)]
fn i2u(i: isize) -> usize { i as usize }

#[repr(transparent)]
pub(super) struct FdOps(pub(super) OwnedFd);
impl FdOps {
    pub(super) unsafe fn read_ptr(
        fd: BorrowedFd<'_>,
        ptr: *mut u8,
        len: usize,
    ) -> io::Result<usize> {
        let bytes_read = unsafe { libc::read(fd.as_raw_fd(), ptr.cast(), len) };
        (bytes_read >= 0).true_val_or_errno(i2u(bytes_read))
    }
    pub(super) fn read(fd: BorrowedFd<'_>, buf: &mut (impl AsBuf + ?Sized)) -> io::Result<usize> {
        unsafe { Self::read_ptr(fd, buf.as_ptr(), buf.len()) }
    }
    pub(super) fn write(fd: BorrowedFd<'_>, buf: &[u8]) -> io::Result<usize> {
        let length_to_write = buf.len();
        let bytes_written =
            unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), length_to_write) };
        (bytes_written >= 0).true_val_or_errno(i2u(bytes_written))
    }
}
impl Read for &FdOps {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { FdOps::read(self.as_fd(), buf) }
    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        let num_bufs = c_int::try_from(bufs.len()).unwrap_or(c_int::MAX);
        let bytes_read =
            unsafe { libc::readv(self.0.as_raw_fd(), bufs.as_ptr().cast(), num_bufs) };
        (bytes_read >= 0).true_val_or_errno(i2u(bytes_read))
    }
    // FUTURE can_vector
}
impl Write for &FdOps {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { FdOps::write(self.as_fd(), buf) }
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let num_bufs = c_int::try_from(bufs.len()).unwrap_or(c_int::MAX);
        let bytes_written =
            unsafe { libc::writev(self.0.as_raw_fd(), bufs.as_ptr().cast(), num_bufs) };
        (bytes_written >= 0).true_val_or_errno(i2u(bytes_written))
    }
    // FUTURE can_vector
    fn flush(&mut self) -> io::Result<()> {
        unsafe { libc::fsync(self.0.as_raw_fd()) >= 0 }.true_val_or_errno(())
    }
}

impl TryClone for FdOps {
    fn try_clone(&self) -> std::io::Result<Self> {
        let fd = c_wrappers::duplicate_fd(self.0.as_fd())?;
        Ok(Self(fd))
    }
}

multimacro! {
    FdOps,
    forward_handle,
    forward_debug,
    derive_raw,
}
