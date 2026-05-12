use std::borrow::Cow;
#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(windows)]
use widestring::U16CStr;

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum NameInner<'s> {
    #[cfg(windows)]
    NamedPipe(Cow<'s, U16CStr>),
    #[cfg(unix)]
    UdSocketPath(Cow<'s, OsStr>),
    #[cfg(unix)]
    UdSocketPseudoNs(Cow<'s, OsStr>),
    #[cfg(any(target_os = "linux", target_os = "android"))]
    UdSocketNs(Cow<'s, [u8]>),
}

impl Default for NameInner<'_> {
    fn default() -> Self {
        // FUTURE make const
        #[cfg(windows)]
        {
            Self::NamedPipe(Cow::default())
        }
        #[cfg(unix)]
        {
            Self::UdSocketPath(Cow::default())
        }
    }
}

macro_rules! map_cow {
    ($nm:ident in $var:expr => $e:expr) => {
        match $var {
            #[cfg(windows)]
            NameInner::NamedPipe($nm) => NameInner::NamedPipe($e),
            #[cfg(unix)]
            NameInner::UdSocketPath($nm) => NameInner::UdSocketPath($e),
            #[cfg(unix)]
            NameInner::UdSocketPseudoNs($nm) => NameInner::UdSocketPseudoNs($e),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            NameInner::UdSocketNs($nm) => NameInner::UdSocketNs($e),
        }
    };
}

impl NameInner<'_> {
    #[inline]
    pub const fn is_namespaced(&self) -> bool {
        match self {
            #[cfg(windows)]
            Self::NamedPipe(..) => true,
            #[cfg(unix)]
            Self::UdSocketPath(..) => false,
            #[cfg(unix)]
            Self::UdSocketPseudoNs(..) => false,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Self::UdSocketNs(..) => true,
        }
    }
    #[inline]
    pub const fn is_path(&self) -> bool {
        match self {
            #[cfg(windows)]
            Self::NamedPipe(..) => true,
            #[cfg(unix)]
            Self::UdSocketPath(..) => true,
            #[cfg(unix)]
            Self::UdSocketPseudoNs(..) => false,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Self::UdSocketNs(..) => false,
        }
    }

    #[inline]
    pub fn borrow(&self) -> NameInner<'_> { map_cow!(cow in self => Cow::Borrowed(cow)) }

    pub fn into_owned(self) -> NameInner<'static> {
        map_cow!(cow in self => Cow::Owned(cow.into_owned()))
    }
}
