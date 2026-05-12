use {
    super::{
        r#type::{NamespacedNameType, PathNameType},
        Name,
    },
    std::{
        borrow::Cow,
        ffi::{CStr, CString, OsStr, OsString},
        io,
        path::{Path, PathBuf},
        str,
    },
};

macro_rules! trivial_string_impl {
    (
        $cvttrait:ident<$str:ident>
        :: $mtd:ident<$nttrait:ident>
        for $($tgt:ty => $via:ident :: $ctor:ident)+
    ) => {$(
        impl<'s> $cvttrait<'s, $str> for $tgt {
            #[inline]
            fn $mtd<T: $nttrait<$str>>(self) -> io::Result<Name<'s>> {
                $via::$ctor(self).$mtd::<T>()
            }
        }
    )+};
}

/// Conversion to a filesystem path-type local socket name.
pub trait ToFsName<'s, S: ToOwned + ?Sized> {
    /// Performs the conversion to a filesystem path-type name.
    ///
    /// Fails if the resulting name isn't supported by the platform.
    fn to_fs_name<NT: PathNameType<S>>(self) -> io::Result<Name<'s>>;
}

/// Conversion to a namespaced local socket name.
pub trait ToNsName<'s, S: ToOwned + ?Sized> {
    /// Performs the conversion to a namespaced name.
    ///
    /// Fails if the resulting name isn't supported by the platform.
    fn to_ns_name<NT: NamespacedNameType<S>>(self) -> io::Result<Name<'s>>;
}

#[allow(dead_code)]
fn err(s: &'static str) -> io::Error { io::Error::new(io::ErrorKind::Unsupported, s) }

impl<'s> ToFsName<'s, OsStr> for &'s Path {
    #[inline]
    fn to_fs_name<FT: PathNameType<OsStr>>(self) -> io::Result<Name<'s>> {
        FT::map(Cow::Borrowed(self.as_os_str()))
    }
}
impl<'s> ToFsName<'s, OsStr> for PathBuf {
    #[inline]
    fn to_fs_name<FT: PathNameType<OsStr>>(self) -> io::Result<Name<'s>> {
        FT::map(Cow::Owned(self.into_os_string()))
    }
}
trivial_string_impl! { ToFsName<OsStr>::to_fs_name<PathNameType> for
    &'s str   => Path   ::new
    String    => PathBuf::from
    &'s OsStr => Path   ::new
    OsString  => PathBuf::from
}

impl<'s> ToNsName<'s, OsStr> for &'s OsStr {
    #[inline]
    fn to_ns_name<NT: NamespacedNameType<OsStr>>(self) -> io::Result<Name<'s>> {
        NT::map(Cow::Borrowed(self))
    }
}
impl<'s> ToNsName<'s, OsStr> for OsString {
    #[inline]
    fn to_ns_name<NT: NamespacedNameType<OsStr>>(self) -> io::Result<Name<'s>> {
        NT::map(Cow::Owned(self))
    }
}
trivial_string_impl! { ToNsName<OsStr>::to_ns_name<NamespacedNameType> for
    &'s str => OsStr   ::new
    String  => OsString::from
}

impl<'s> ToFsName<'s, CStr> for &'s CStr {
    #[inline]
    fn to_fs_name<FT: PathNameType<CStr>>(self) -> io::Result<Name<'s>> {
        FT::map(Cow::Borrowed(self))
    }
}
impl<'s> ToFsName<'s, CStr> for CString {
    #[inline]
    fn to_fs_name<FT: PathNameType<CStr>>(self) -> io::Result<Name<'s>> {
        FT::map(Cow::Owned(self))
    }
}

impl<'s> ToNsName<'s, CStr> for &'s CStr {
    #[inline]
    fn to_ns_name<NT: NamespacedNameType<CStr>>(self) -> io::Result<Name<'s>> {
        NT::map(Cow::Borrowed(self))
    }
}
impl<'s> ToNsName<'s, CStr> for CString {
    #[inline]
    fn to_ns_name<NT: NamespacedNameType<CStr>>(self) -> io::Result<Name<'s>> {
        NT::map(Cow::Owned(self))
    }
}
