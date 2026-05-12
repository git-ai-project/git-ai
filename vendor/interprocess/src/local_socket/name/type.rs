//! Construction of local socket names, facilitating local socket implementation dispatch.
//!
//! The traits [`PathNameType`] and [`NamespacedNameType`] are implemented on uninhabited tag types
//! and are to be used via [`ToFsName`](super::ToFsName) and [`ToNsName`](super::ToNsName)
//! respectively. They are also sealed (cannot be implemented outside of Interprocess).
//!
//! The name type you choose affects what local socket implementation will be used. See the
//! documentation on the tag types to learn more.

#[cfg(unix)]
use std::ffi::CStr;
use {
    super::Name,
    crate::Sealed,
    std::{borrow::Cow, ffi::OsStr, io},
};

impmod! {local_socket::name_type as n_impl}

/// Mappings from string types to [local socket names](Name).
///
/// Types that implement this trait are [uninhabited] type-level markers: those which implement
/// [`PathNameType`] serve as generic arguments for
/// [`ToFsName::to_fs_name()`](super::ToFsName::to_fs_name), while those which implement
/// [`NamespacedNameType`] are used with [`ToNsName::to_ns_name()`](super::ToNsName::to_ns_name).
///
/// [uninhabited]: https://doc.rust-lang.org/reference/glossary.html#uninhabited
///
/// **It is a breaking change for a mapping to meaningfully change.** More concretely, if a name
/// produced by this mapping from some input results in a valid listener via
/// [server creation](super::super::ListenerOptions) or successfully locates one via
/// [client creation](super::super::traits::Stream::connect), the name type will continue to map
/// that input to the same name, for the OS's definition of "same".
#[allow(private_bounds)]
pub trait NameType: Copy + std::fmt::Debug + Eq + Send + Sync + Unpin + Sealed {
    /// Whether the name type is supported within the runtime circumstances of the program.
    ///
    /// May entail querying support status from the OS, returning `false` in the event of an OS
    /// error.
    fn is_supported() -> bool;
}

/// [Mappings](NameType) from paths to [local socket names](Name).
///
/// See [`ToFsName::to_fs_name()`](super::ToFsName::to_fs_name).
pub trait PathNameType<S: ToOwned + ?Sized>: NameType {
    /// Maps the given path to a local socket name, failing if the resulting name is unsupported by
    /// the underlying OS.
    ///
    /// The idiomatic way to use this is [`ToFsName::to_fs_name()`](super::ToFsName::to_fs_name).
    fn map(path: Cow<'_, S>) -> io::Result<Name<'_>>;
}
/// [Mappings](NameType) from strings to [local socket names](Name).
///
/// See [`ToNsName::to_ns_name()`](super::ToNsName::to_ns_name).
pub trait NamespacedNameType<S: ToOwned + ?Sized>: NameType {
    /// Maps the given string to a local socket name, failing if the resulting name is unsupported
    /// by the underlying OS.
    ///
    /// The idiomatic way to use this is [`ToNsName::to_ns_name()`](super::ToNsName::to_ns_name).
    fn map(name: Cow<'_, S>) -> io::Result<Name<'_>>;
}

tag_enum!(
/// Consistent platform-specific mapping from filesystem paths to local socket names.
///
/// This name type, like [`GenericNamespaced`] is designed to be always supported on all platforms,
/// whatever it takes. What follows below is a complete description of how that is implemented.
///
/// ## Platform-specific behavior
/// ### Windows
/// For paths that start with `\\.\pipe\`, maps them to named pipe names without performing any
/// transformations. Attempting to map any other type of path, including a normalization-bypassing
/// path (`\\?\`) currently returns an error.
///
/// ### Unix
/// Resolves to filesystem paths to Unix domain sockets without performing any transformations.
GenericFilePath);
impl NameType for GenericFilePath {
    fn is_supported() -> bool { true }
}
impl PathNameType<OsStr> for GenericFilePath {
    #[inline]
    fn map(path: Cow<'_, OsStr>) -> io::Result<Name<'_>> { n_impl::map_generic_path_osstr(path) }
}
#[cfg(unix)]
#[cfg_attr(feature = "doc_cfg", doc(cfg(unix)))]
impl PathNameType<CStr> for GenericFilePath {
    #[inline]
    fn map(path: Cow<'_, CStr>) -> io::Result<Name<'_>> { n_impl::map_generic_path_cstr(path) }
}

tag_enum!(
/// Consistent platform-specific mapping from arbitrary OS strings to local socket names.
///
/// This name type, like [`GenericFilePath`] is designed to be always supported on all platforms,
/// whatever it takes. What follows below is a complete description of how that is implemented.
///
/// ## Platform-specific behavior
/// ### Windows
/// Resolves to named pipe names by prepending `\\.\pipe\` (thus, only local named pipes are
/// addressable).
///
/// ### Linux
/// Resolves to the abstract namespace with no string transformations and thus has a maximum length
/// of 107 bytes. (This is the behavior of `AbstractNsUdSocket.`)
///
/// ### Other Unices
/// Resolves to filesystem paths by prepending `/tmp/` (but see `SpecialDirUdSocket` and its
/// deprecation warning).
GenericNamespaced);
impl NameType for GenericNamespaced {
    fn is_supported() -> bool { true }
}
impl NamespacedNameType<OsStr> for GenericNamespaced {
    #[inline]
    fn map(name: Cow<'_, OsStr>) -> io::Result<Name<'_>> {
        n_impl::map_generic_namespaced_osstr(name)
    }
}
#[cfg(unix)]
#[cfg_attr(feature = "doc_cfg", doc(cfg(unix)))]
impl NamespacedNameType<CStr> for GenericNamespaced {
    #[inline]
    fn map(name: Cow<'_, CStr>) -> io::Result<Name<'_>> {
        n_impl::map_generic_namespaced_cstr(name)
    }
}
