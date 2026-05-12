//! Tokio-based asynchronous unnamed pipes.
//!
//! See the [parent-level documentation](super) for more.
//!
//! # Examples
//! See [`pipe()`].

impmod! {unnamed_pipe::tokio,
    Recver as RecverImpl,
    Sender as SenderImpl,
    pipe_impl,
}
use std::io;

/// Creates a new pipe with the default creation settings and returns Tokio-based handles to its
/// sending end and receiving end.
///
/// The platform-specific builders in the `os` module of the crate might be more helpful if extra
/// configuration for the pipe is needed.
///
/// # Examples
/// ## Basic communication
/// In a parent process, within a Tokio runtime:
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/unnamed_pipe/sync/side_a.rs"))]
/// ```
/// In a child process, within a Tokio runtime:
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/unnamed_pipe/sync/side_b.rs"))]
/// ```
#[inline]
pub fn pipe() -> io::Result<(Sender, Recver)> { pipe_impl() }

/// Tokio-based handle to the receiving end of an unnamed pipe, created by the [`pipe()`] function
/// together with the [sending end](Sender).
///
/// The core functionality is exposed via the [`AsyncRead`](tokio::io::AsyncRead) trait. The type
/// is convertible to and from handles/file descriptors and allows its internal handle/FD to be
/// borrowed. On Windows, the `ShareHandle` trait is also implemented.
///
/// The handle/file descriptor is inheritable. See [module-level documentation](self) for more on
/// how this can be used.
// field is pub(crate) to allow platform builders to create the public-facing pipe types
pub struct Recver(pub(crate) RecverImpl);
multimacro! {
    Recver,
    pinproj_for_unpin(RecverImpl),
    forward_tokio_read,
    forward_as_handle,
    forward_try_handle(io::Error),
    forward_debug,
    derive_asraw,
}

/// Handle to the sending end of an unnamed pipe, created by the [`pipe()`] function together with
/// the [receiving end](Recver).
///
/// The core functionality is exposed via the [`AsyncWrite`](tokio::io::AsyncWrite) trait. The
/// type is convertible to and from handles/file descriptors and allows its internal handle/FD to
/// be borrowed. On Windows, the `ShareHandle` trait is also implemented.
///
/// The handle/file descriptor is inheritable. See [module-level documentation](self) for more on
/// how this can be used.
pub struct Sender(pub(crate) SenderImpl);
multimacro! {
    Sender,
    pinproj_for_unpin(SenderImpl),
    forward_rbv(SenderImpl, &),
    forward_tokio_write,
    forward_as_handle,
    forward_try_handle(io::Error),
    forward_debug,
    derive_asraw,
}
