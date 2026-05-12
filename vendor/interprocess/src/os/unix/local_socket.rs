//! Unix-specific local socket features.

pub(crate) mod dispatch_sync;
#[cfg(feature = "tokio")]
pub(crate) mod dispatch_tokio;
pub(crate) mod name_type;
pub(crate) mod peer_creds;

use crate::{local_socket::ListenerOptions, Sealed};
pub use name_type::*;

/// Unix-specific [listener options](ListenerOptions).
#[allow(private_bounds)]
pub trait ListenerOptionsExt: Sized + Sealed {
    /// Sets the file mode (Unix permissions) to be applied to the socket file. This will
    /// authenticate clients using their process credentials according to the **write bits** of
    /// the mode; the read and execute bits are ignored by the OS and serve a cosmetic purpose.
    ///
    /// # Platform support
    /// The following platforms are known to support this feature:
    /// - Linux
    /// - FreeBSD 14.3 (2025-06-10) and newer
    /// - OpenBSD
    ///
    /// Other Unix systems may support this as well.
    /// [`Unsupported`](std::io::ErrorKind::Unsupported) will be returned on platforms that are
    /// (dynamically) determined to not support choice of file mode of socket listeners.
    ///
    /// # Implementation notes
    /// An `fchmod()` is performed on the socket prior to `bind()` and `listen()`. This eliminates
    /// what would otherwise be a umask race condition. Previous versions of Interprocess made use
    /// of a racy fallback, but it did not receive adoption, and was removed in 2.3.0.
    #[must_use = builder_must_use!()]
    fn mode(self, mode: libc::mode_t) -> Self;
}

impl ListenerOptionsExt for ListenerOptions<'_> {
    #[inline(always)]
    fn mode(mut self, mode: libc::mode_t) -> Self {
        self.set_mode(mode);
        self
    }
}
