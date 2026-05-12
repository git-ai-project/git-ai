use {
    super::super::{dispatch_name, CONN_TIMEOUT_MSG},
    crate::{
        error::ReuniteError,
        local_socket::{
            traits::{tokio as traits, StreamCommon},
            ConnectOptions, PeerCreds,
        },
        os::unix::{c_wrappers, local_socket::peer_creds::PeerCreds as PeerCredsInner},
        ConnectWaitMode, Sealed,
    },
    std::{
        io::{self, ErrorKind::WouldBlock},
        os::{
            fd::{AsFd, OwnedFd},
            unix::{net::UnixStream as SyncUnixStream, prelude::BorrowedFd},
        },
        pin::Pin,
        task::{ready, Context, Poll},
    },
    tokio::{
        io::{AsyncRead, AsyncWrite, ReadBuf},
        net::{
            unix::{OwnedReadHalf as RecvHalfImpl, OwnedWriteHalf as SendHalfImpl},
            UnixStream,
        },
    },
};

/// Wrapper around [`UnixStream`] that implements [`Stream`](traits::Stream).
#[derive(Debug)]
pub struct Stream(pub(super) UnixStream);
impl Sealed for Stream {}

impl traits::Stream for Stream {
    type RecvHalf = RecvHalf;
    type SendHalf = SendHalf;

    async fn from_options(mut opts: &ConnectOptions<'_>) -> io::Result<Self> {
        let (sock, inprog) = dispatch_name(
            &mut opts,
            false,
            |&mut opts| opts.name.borrow(),
            |_| None,
            |addr, _| c_wrappers::create_client(addr, true),
        )?;
        let sock = UnixStream::from_std(SyncUnixStream::from(sock))?;
        if inprog {
            // disapprovingly points finger at Mio
            match opts.get_wait_mode() {
                ConnectWaitMode::Deferred => {}
                ConnectWaitMode::Timeout(timeout) => tokio::select! {
                    biased;
                    rslt = sock.writable() => rslt,
                    _ = tokio::time::sleep(timeout) => {
                        Err(io::Error::new(io::ErrorKind::TimedOut, CONN_TIMEOUT_MSG))
                    }
                }?,
                ConnectWaitMode::Unbounded => sock.writable().await?,
            }
        }
        Ok(Self(sock))
    }
    fn split(self) -> (RecvHalf, SendHalf) {
        let (r, w) = self.0.into_split();
        (RecvHalf(r), SendHalf(w))
    }
    #[inline]
    fn reunite(rh: RecvHalf, sh: SendHalf) -> Result<Self, ReuniteError<RecvHalf, SendHalf>> {
        rh.0.reunite(sh.0).map(Self::from).map_err(|tokio::net::unix::ReuniteError(rh, sh)| {
            ReuniteError { rh: RecvHalf(rh), sh: SendHalf(sh) }
        })
    }
}
impl StreamCommon for Stream {
    #[inline]
    fn take_error(&self) -> io::Result<Option<io::Error>> { c_wrappers::take_error(self.as_fd()) }
    #[inline]
    fn peer_creds(&self) -> io::Result<PeerCreds> {
        PeerCredsInner::for_socket(self.as_fd()).map(From::from)
    }
}

/// Access to the underlying implementation.
impl Stream {
    /// Borrows the [`UnixStream`] contained within, granting access to operations defined on it.
    #[inline(always)]
    pub fn inner(&self) -> &UnixStream { &self.0 }
    /// Mutably borrows the [`UnixStream`] contained within, granting access to operations defined
    /// on it.
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut UnixStream { &mut self.0 }
}

fn ioloop(
    mut try_io: impl FnMut() -> io::Result<usize>,
    mut poll_read_ready: impl FnMut() -> Poll<io::Result<()>>,
) -> Poll<io::Result<usize>> {
    loop {
        match try_io() {
            Err(e) if e.kind() == WouldBlock => ready!(poll_read_ready()?),
            els => return Poll::Ready(els),
        };
    }
}

multimacro! {
    Stream,
    pinproj_for_unpin(UnixStream),
    forward_rbv(UnixStream, &),
    forward_tokio_rw,
    forward_as_handle(unix),
    derive_trivial_conv(UnixStream),
}
impl AsyncRead for &Stream {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        ioloop(|| self.0.try_read_buf(buf), || self.0.poll_read_ready(cx)).map(|e| e.map(|_| ()))
    }
}
impl AsyncWrite for &Stream {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ioloop(|| self.0.try_write(buf), || self.0.poll_write_ready(cx))
    }
    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        ioloop(|| self.0.try_write_vectored(bufs), || self.0.poll_write_ready(cx))
    }
    #[inline]
    fn is_write_vectored(&self) -> bool { self.0.is_write_vectored() }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl TryFrom<Stream> for OwnedFd {
    type Error = io::Error;
    #[inline]
    fn try_from(slf: Stream) -> io::Result<Self> { Ok(slf.0.into_std()?.into()) }
}
impl TryFrom<OwnedFd> for Stream {
    type Error = io::Error;
    #[inline]
    fn try_from(fd: OwnedFd) -> io::Result<Self> {
        Ok(UnixStream::from_std(SyncUnixStream::from(fd))?.into())
    }
}

macro_rules! tokio_accessors {
    ($ty:ty, $inner:ty) => {
        /// Tokio accessors.
        impl $ty {
            /// Borrows the underlying Tokio object, granting access to its methods.
            #[inline]
            pub fn as_tokio(&self) -> &$inner { &self.0 }
            /// Extracts the underlying Tokio object.
            #[inline]
            pub fn into_tokio(self) -> $inner { self.0 }
        }
    };
}

/// [`Stream`]'s receive half, internally implemented using [`Arc`](std::sync::Arc) by Tokio.
pub struct RecvHalf(RecvHalfImpl);
impl Sealed for RecvHalf {}
multimacro! {
    RecvHalf,
    pinproj_for_unpin(RecvHalfImpl),
    tokio_accessors(RecvHalfImpl),
    forward_debug("local_socket::RecvHalf"),
    forward_tokio_read,
}
impl traits::RecvHalf for RecvHalf {
    type Stream = Stream;
}
impl AsyncRead for &RecvHalf {
    #[inline]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        ioloop(|| self.0.try_read_buf(buf), || self.0.as_ref().poll_read_ready(cx))
            .map(|e| e.map(|_| ()))
    }
}
impl AsFd for RecvHalf {
    #[inline]
    fn as_fd(&self) -> BorrowedFd<'_> { self.0.as_ref().as_fd() }
}

/// [`Stream`]'s send half, internally implemented using [`Arc`](std::sync::Arc) by Tokio.
pub struct SendHalf(SendHalfImpl);
impl Sealed for SendHalf {}
multimacro! {
    SendHalf,
    pinproj_for_unpin(SendHalfImpl),
    tokio_accessors(SendHalfImpl),
    forward_rbv(SendHalfImpl, &),
    forward_debug("local_socket::SendHalf"),
    forward_tokio_write,
}
impl traits::SendHalf for SendHalf {
    type Stream = Stream;
}
impl AsyncWrite for &SendHalf {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ioloop(|| self.0.try_write(buf), || self.0.as_ref().poll_write_ready(cx))
    }
    #[inline]
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        ioloop(|| self.0.try_write_vectored(bufs), || self.0.as_ref().poll_write_ready(cx))
    }
    #[inline]
    fn is_write_vectored(&self) -> bool { self.0.is_write_vectored() }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
impl AsFd for SendHalf {
    #[inline]
    fn as_fd(&self) -> BorrowedFd<'_> { self.0.as_ref().as_fd() }
}
