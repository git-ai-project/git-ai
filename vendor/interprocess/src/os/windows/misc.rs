#[allow(unused_imports)]
pub(crate) mod winprelude {
    pub(in super::super) use super::super::{adv_handle::*, linger_pool};
    pub(crate) use {
        std::os::windows::prelude::*,
        windows_sys::{core::BOOL, Win32::Foundation::INVALID_HANDLE_VALUE},
    };
}

use {
    crate::RawOsErrorExt as _,
    std::io::{self, ErrorKind::BrokenPipe},
};

pub(super) fn decode_eof<T>(r: io::Result<T>) -> io::Result<T> {
    use windows_sys::Win32::Foundation::ERROR_PIPE_NOT_CONNECTED;
    match r {
        Err(e) if e.raw_os_error().eeq(ERROR_PIPE_NOT_CONNECTED) => {
            Err(io::Error::from(BrokenPipe))
        }
        els => els,
    }
}
pub(super) fn downgrade_eof<T: Default>(r: io::Result<T>) -> io::Result<T> {
    match decode_eof(r) {
        Err(e) if e.kind() == BrokenPipe => Ok(T::default()),
        els => els,
    }
}
