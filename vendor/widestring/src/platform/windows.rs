use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

pub(crate) fn os_to_wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().collect()
}

pub(crate) fn os_from_wide(s: &[u16]) -> OsString {
    OsString::from_wide(s)
}
