use {
    super::{dispatch_name, CONN_TIMEOUT_MSG},
    crate::{
        error::ReuniteError,
        local_socket::{
            prelude::*,
            traits::{self, ReuniteResult},
            ConnectOptions, PeerCreds,
        },
        os::unix::{
            c_wrappers, local_socket::peer_creds::PeerCreds as PeerCredsInner, unixprelude::*,
        },
        ConnectWaitMode, Sealed, TryClone,
    },
    std::{
        io::{self, prelude::*, IoSlice, IoSliceMut},
        os::unix::net::UnixStream,
        sync::Arc,
        time::Duration,
    },
};

/// Wrapper around [`UnixStream`] that implements [`Stream`](traits::Stream).
#[derive(Debug)]
pub struct Stream(pub(super) UnixStream);
impl Sealed for Stream {}
impl traits::Stream for Stream {
    type RecvHalf = RecvHalf;
    type SendHalf = SendHalf;

    fn from_options(mut opts: &ConnectOptions<'_>) -> io::Result<Self> {
        let nonblocking_connect = matches!(
            opts.get_wait_mode(),
            ConnectWaitMode::Timeout(..) | ConnectWaitMode::Deferred
        );
        let (stream, inprog) = dispatch_name(
            &mut opts,
            false,
            |&mut opts| opts.name.borrow(),
            |_| None,
            |addr, _| c_wrappers::create_client(addr, nonblocking_connect),
        )?;
        if let ConnectWaitMode::Timeout(timeout) = opts.get_wait_mode() {
            if inprog {
                c_wrappers::wait_for_connect(stream.as_fd(), Some(timeout), CONN_TIMEOUT_MSG)?;
            }
        }
        if opts.get_nonblocking_stream() != nonblocking_connect {
            c_wrappers::fast_set_nonblocking(stream.as_fd(), opts.get_nonblocking_stream())?;
        }
        Ok(stream.into())
    }

    #[inline]
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        c_wrappers::set_nonblocking(self.as_fd(), nonblocking)
    }

    #[inline]
    fn set_recv_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }
    #[inline]
    fn set_send_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }

    #[inline]
    fn split(self) -> (RecvHalf, SendHalf) {
        let arc = Arc::new(self);
        (RecvHalf(Arc::clone(&arc)), SendHalf(arc))
    }
    #[inline]
    #[allow(clippy::unwrap_in_result)]
    fn reunite(rh: RecvHalf, sh: SendHalf) -> ReuniteResult<Self> {
        if !Arc::ptr_eq(&rh.0, &sh.0) {
            return Err(ReuniteError { rh, sh });
        }
        drop(rh);
        let inner = Arc::into_inner(sh.0).expect("stream half inexplicably copied");
        Ok(inner)
    }
}
impl traits::StreamCommon for Stream {
    #[inline]
    fn take_error(&self) -> io::Result<Option<io::Error>> { c_wrappers::take_error(self.as_fd()) }
    #[inline]
    fn peer_creds(&self) -> io::Result<PeerCreds> {
        PeerCredsInner::for_socket(self.as_fd()).map(From::from)
    }
}

impl Read for &Stream {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { (&mut &self.0).read(buf) }
    #[inline]
    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        (&mut &self.0).read_vectored(bufs)
    }
    // FUTURE is_read_vectored
}
impl Write for &Stream {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { (&mut &self.0).write(buf) }
    #[inline]
    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        (&mut &self.0).write_vectored(bufs)
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
    // FUTURE is_write_vectored
}

/// Access to the underlying implementation.
impl Stream {
    /// Borrows the [`UnixStream`] contained within, granting access to operations defined on it.
    #[inline(always)]
    pub fn inner(&self) -> &UnixStream { &self.0 }
    /// Mutably borrows the [`UnixStream`] contained within, granting access to operations defined
    /// on it.
    ///
    /// This may allow for non-portable concurrent I/O. Please use [`inner`](Self::inner) instead
    /// if you can.
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut UnixStream { &mut self.0 }
}

impl From<UnixStream> for Stream {
    #[inline]
    fn from(s: UnixStream) -> Self { Self(s) }
}

impl From<OwnedFd> for Stream {
    #[inline]
    fn from(fd: OwnedFd) -> Self { UnixStream::from(fd).into() }
}

impl TryClone for Stream {
    #[inline]
    fn try_clone(&self) -> std::io::Result<Self> { self.0.try_clone().map(Self::from) }
}

multimacro! {
    Stream,
    forward_asinto_handle(unix),
    derive_sync_mut_rw,
}

macro_rules! arc_accessors {
    ($ty:ty) => {
        /// [`Arc`] accessors.
        impl $ty {
            /// Borrows the [`Stream`] within the `Arc`.
            #[inline]
            pub fn as_stream(&self) -> &Stream { &self.0 }
            /// Extracts the underlying `Arc<Stream>`.
            #[inline]
            pub fn into_arc(self) -> Arc<Stream> { self.0 }
            /// Borrows the underlying `Arc<Stream>`, granting access to extra information about
            /// the `Arc`.
            #[inline]
            pub fn as_arc(&self) -> &Arc<Stream> { &self.0 }
        }
    };
}

/// [`Stream`]'s receive half, implemented using [`Arc`].
#[derive(Clone, Debug)]
pub struct RecvHalf(pub(super) Arc<Stream>);
impl Sealed for RecvHalf {}
multimacro! {
    RecvHalf,
    forward_rbv(Stream, *),
    arc_accessors,
    forward_sync_ref_read,
    forward_as_handle,
    derive_sync_mut_read,
}
impl traits::RecvHalf for RecvHalf {
    type Stream = Stream;

    #[inline]
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_recv_timeout(timeout)
    }
}

/// [`Stream`]'s send half, implemented using [`Arc`].
#[derive(Clone, Debug)]
pub struct SendHalf(pub(super) Arc<Stream>);
impl Sealed for SendHalf {}
multimacro! {
    SendHalf,
    forward_rbv(Stream, *),
    arc_accessors,
    forward_sync_ref_write,
    forward_as_handle,
    derive_sync_mut_write,
}
impl traits::SendHalf for SendHalf {
    type Stream = Stream;

    #[inline]
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_send_timeout(timeout)
    }
}
