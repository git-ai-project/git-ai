use std::ffi::{OsStr, OsString};

pub(crate) fn os_to_wide(s: &OsStr) -> Vec<u16> {
    s.to_string_lossy().encode_utf16().collect()
}

pub(crate) fn os_from_wide(s: &[u16]) -> OsString {
    OsString::from(String::from_utf16_lossy(s))
}
