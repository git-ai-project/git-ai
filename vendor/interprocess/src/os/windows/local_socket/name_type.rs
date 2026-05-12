use {
    crate::{
        local_socket::{Name, NameInner, NameType, PathNameType},
        os::windows::{convert_and_encode_path, convert_osstr},
    },
    std::{borrow::Cow, ffi::OsStr, io},
};

tag_enum!(
/// [Mapping](NameType) that produces
/// [named pipe local socket](crate::os::windows::named_pipe::local_socket) names.
///
/// Named pipe paths of the form `\\HOSTNAME\pipe\PIPENAME` are passed through verbatim. Other paths
/// yield an error, as they do not point to NPFS.
///
/// Namespaced strings have `\\.\pipe\` prepended to them – using
/// [`ToNsName`](crate::local_socket::ToNsName) conversions implies the hostname `.`, which is the
/// local system.
NamedPipe);
impl NameType for NamedPipe {
    fn is_supported() -> bool { true }
}
impl PathNameType<OsStr> for NamedPipe {
    fn map(path: Cow<'_, OsStr>) -> io::Result<Name<'_>> {
        if !is_pipefs(&path) {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "not a named pipe path"));
        }
        Ok(Name(NameInner::NamedPipe(Cow::Owned(convert_osstr(&path)?))))
    }
}

pub(crate) fn map_generic_path_osstr(path: Cow<'_, OsStr>) -> io::Result<Name<'_>> {
    // TODO do something meaningful for non-NPFS paths instead of rejecting them
    // TODO normskip (`\\?\`) paths
    NamedPipe::map(path)
}

pub(crate) fn map_generic_namespaced_osstr(name: Cow<'_, OsStr>) -> io::Result<Name<'_>> {
    // The prepending currently happens at a later point.
    Ok(Name(NameInner::NamedPipe(Cow::Owned(convert_and_encode_path(&name, None)?))))
}

#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)] // minlen check
fn is_pipefs(slf: &OsStr) -> bool {
    const PFX1: &[u8] = br"\\";
    const PFX2: &[u8] = br"\pipe\";
    const LEN_PFX1: usize = PFX1.len();
    const LEN_PFX2: usize = PFX2.len();
    const MINLEN: usize = LEN_PFX1 + LEN_PFX2 + 1;

    let b = slf.as_encoded_bytes();
    if (b.len() < MINLEN) || (&b[..LEN_PFX1] != PFX1) {
        return false;
    }
    let Some(slashidx) = findslash(&b[LEN_PFX1..]) else {
        return false;
    };
    let hostbase = LEN_PFX1 + slashidx;
    &b[hostbase..(hostbase + LEN_PFX2)] == PFX2
}

#[inline]
fn findslash(slice: &[u8]) -> Option<usize> {
    for (i, e) in slice.iter().copied().enumerate() {
        if e == b'\\' {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    static NP: &str = r"Именованная труба\yeah";
    #[track_caller]
    fn assert_pipefs(s: impl AsRef<OsStr>) {
        assert!(is_pipefs(s.as_ref()));
    }
    #[track_caller]
    fn assert_not_pipefs(s: impl AsRef<OsStr>) {
        assert!(!is_pipefs(s.as_ref()));
    }

    #[test]
    fn local() { assert_pipefs(format!(r"\\.\pipe\{NP}")); }
    #[test]
    fn remote() { assert_pipefs(format!(r"\\CHARA\pipe\{NP}")); }

    #[test]
    fn bad() { assert_not_pipefs("iwiwiwiwiwiwiwiwiwiwiwiwi"); }
    #[test]
    fn can_not_do_unix_things() { assert_not_pipefs(r"C:\Users\GetSilly\neovide.sock"); }
}
