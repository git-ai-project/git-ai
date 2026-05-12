mod inner;
pub(super) mod to_name;
pub(super) mod r#type;

pub(crate) use self::inner::*;
pub use {r#type::*, to_name::*};

/// Name for a local socket.
///
/// Due to significant differences between how different platforms name local sockets, there needs
/// to be a way to store and process those in a unified way while also retaining those
/// platform-specific pecularities. `Name` exists to bridge the gap between portability and
/// correctness, minimizing the amount of platform-dependent code in downstream programs.
///
/// # Creation
/// Two traits are used to create names from basic strings: [`ToFsName`](super::ToFsName) and
/// [`ToNsName`](super::ToNsName).
///
/// # Validity
/// As mentioned in the [module-level documentation](super), not all platforms support all types of
/// local socket names. Names pointing to filesystem locations are only supported on Unix-like
/// systems, and names pointing to an abstract namespace reserved specifically for local sockets are
/// only available on Linux and Windows.
///
/// Instances of this type cannot be constructed from unsupported values. They can, however, be
/// constructed from invalid ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name<'s>(pub(crate) NameInner<'s>);
impl Name<'_> {
    /// Returns `true` if the name points to a dedicated local socket namespace, `false` otherwise.
    #[inline]
    pub fn is_namespaced(&self) -> bool { self.0.is_namespaced() }

    /// Returns `true` if the name is stored as a filesystem path, `false` otherwise.
    ///
    /// Note that it is possible for [`.is_namespaced()`](Self::is_namespaced) and `.is_path()` to
    /// return `true` simultaneously:
    /// ```
    /// # #[cfg(windows)] {
    /// use interprocess::{local_socket::ToFsName, os::windows::local_socket::NamedPipe};
    /// let name = r"\\.\pipe\example".to_fs_name::<NamedPipe>().unwrap();
    /// assert!(name.is_namespaced()); // \\.\pipe\ is a namespace
    /// assert!(name.is_path());       // \\.\pipe\example is a path
    /// # }
    /// ```
    #[inline]
    pub const fn is_path(&self) -> bool { self.0.is_path() }

    /// Produces a `Name` that borrows from `self`.
    #[inline]
    pub fn borrow(&self) -> Name<'_> { Name(self.0.borrow()) }

    /// Extends the lifetime to `'static`, cloning if necessary.
    #[inline]
    pub fn into_owned(self) -> Name<'static> { Name(self.0.into_owned()) }

    pub(crate) fn invalid() -> Self { Self(NameInner::default()) }
}
