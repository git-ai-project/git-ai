#[cfg(unix)]
use crate::os::unix::uds_local_socket as uds_impl;
#[cfg(windows)]
use crate::os::windows::named_pipe::local_socket as np_impl;
use {
    super::{options::ListenerOptions, r#trait},
    crate::local_socket::{ListenerNonblockingMode, Stream},
    std::{io, iter::FusedIterator},
};

impmod! {local_socket::dispatch_sync as dispatch}

mkenum!(
/// Local socket server, listening for connections.
///
/// This struct is created by [`ListenerOptions`](super::options::ListenerOptions).
///
/// See the [module-level documentation](crate::local_socket) for more details.
///
/// # Name reclamation
/// *This section only applies to Unix domain sockets.*
///
/// When a Unix domain socket listener is closed, its associated socket file is not automatically
/// deleted. Instead, it remains on the filesystem in a zombie state, neither accepting connections
/// nor allowing a new listener to reuse it – [`create_sync()`] will return
/// [`AddrInUse`](io::ErrorKind::AddrInUse) on some platforms unless the detached socket file is
/// deleted manually.
///
/// Interprocess implements *automatic name reclamation*: when the local socket listener is
/// dropped, it performs [`std::fs::remove_file()`] (i.e. `unlink()`) with the path that was
/// originally passed to [`create_sync()`], allowing for subsequent reuse of the local socket name.
///
/// If the program crashes in a way that doesn't unwind the stack, the deletion will not occur and
/// the socket file will linger on the filesystem, in which case manual deletion will be necessary.
/// Identially, the automatic name reclamation mechanism can be opted out of via
/// [`.do_not_reclaim_name_on_drop()`](trait::Listener::do_not_reclaim_name_on_drop) on the listener
/// or [`.reclaim_name(false)`](super::options::ListenerOptions::reclaim_name) on the builder.
///
/// Note that the socket file can be unlinked by other programs at any time, retaining the inode the
/// listener is bound to but making it inaccessible to peers if it was at its last hardlink. If that
/// happens and another listener takes the same path before the first one performs name reclamation,
/// the socket file deletion wouldn't correspond to the listener being closed, instead deleting the
/// socket file of the second listener. If the second listener also performs name reclamation, the
/// ensuing deletion will silently fail. Due to the awful design of Unix, this issue cannot be
/// mitigated.
///
/// [`create_sync()`]: super::options::ListenerOptions::create_sync
///
/// # Examples
///
/// ## Basic server
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/local_socket/sync/listener.rs"))]
/// ```
Listener);

impl r#trait::Listener for Listener {
    type Stream = Stream;

    #[inline]
    fn from_options(options: ListenerOptions<'_>) -> io::Result<Self> {
        dispatch::listen(options)
    }
    #[inline]
    fn accept(&self) -> io::Result<Stream> {
        dispatch!(Self: x in self => x.accept()).map(Stream::from)
    }
    #[inline]
    fn set_nonblocking(&self, nonblocking: ListenerNonblockingMode) -> io::Result<()> {
        dispatch!(Self: x in self => x.set_nonblocking(nonblocking))
    }
    #[inline]
    fn do_not_reclaim_name_on_drop(&mut self) {
        dispatch!(Self: x in self => x.do_not_reclaim_name_on_drop())
    }
}
impl Iterator for Listener {
    type Item = io::Result<Stream>;
    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> { Some(r#trait::Listener::accept(self)) }
}
impl FusedIterator for Listener {}
