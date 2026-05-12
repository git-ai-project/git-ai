#[cfg(unix)]
use crate::os::unix::uds_local_socket::tokio as uds_impl;
#[cfg(windows)]
use crate::os::windows::named_pipe::local_socket::tokio as np_impl;
use {
    super::r#trait,
    crate::local_socket::{traits::StreamCommon, ConnectOptions, PeerCreds},
    std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    },
    tokio::io::{AsyncRead, AsyncWrite, ReadBuf},
};

impmod! {local_socket::dispatch_tokio as dispatch}

macro_rules! dispatch_read {
    (@iw $ty:ident) => {
        #[inline]
        fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
            dispatch!($ty: x in self.get_mut() => Pin::new(x).poll_read(cx, buf))
        }
    };
    ($ty:ident) => {
        impl AsyncRead for &$ty {
            dispatch_read!(@iw $ty);
        }
        impl AsyncRead for $ty {
            dispatch_read!(@iw $ty);
        }
    };
}
macro_rules! dispatch_write {
    (@iw $ty:ident) => {
        #[inline]
        fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
            dispatch!($ty: x in self.get_mut() => Pin::new(x).poll_write(cx, buf))
        }
        #[inline]
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        #[inline]
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    };
    ($ty:ident) => {
        /// Flushing and shutdown are always successful no-ops.
        impl AsyncWrite for &$ty {
            dispatch_write!(@iw $ty);
        }
        /// Flushing and shutdown are always successful no-ops.
        impl AsyncWrite for $ty {
            dispatch_write!(@iw $ty);
        }
    };
}

mkenum!(
/// Tokio-based local socket byte stream, obtained either from [`Listener`](super::super::Listener)
/// or by connecting to an existing local socket.
///
/// See the [module-level documentation of local sockets](crate::local_socket) for more details.
///
/// # Examples
///
/// ## Basic client
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/local_socket/tokio/listener.rs"))]
/// ```
Stream);

impl r#trait::Stream for Stream {
    type RecvHalf = RecvHalf;
    type SendHalf = SendHalf;

    #[inline]
    async fn from_options(options: &ConnectOptions<'_>) -> io::Result<Self> {
        dispatch::connect(options).await
    }
    fn split(self) -> (RecvHalf, SendHalf) {
        match self {
            #[cfg(windows)]
            Stream::NamedPipe(s) => {
                let (rh, sh) = s.split();
                (RecvHalf::NamedPipe(rh), SendHalf::NamedPipe(sh))
            }
            #[cfg(unix)]
            Stream::UdSocket(s) => {
                let (rh, sh) = s.split();
                (RecvHalf::UdSocket(rh), SendHalf::UdSocket(sh))
            }
        }
    }
    fn reunite(rh: RecvHalf, sh: SendHalf) -> ReuniteResult {
        match (rh, sh) {
            #[cfg(windows)]
            (RecvHalf::NamedPipe(rh), SendHalf::NamedPipe(sh)) => {
                np_impl::Stream::reunite(rh, sh).map(From::from).map_err(|e| e.convert_halves())
            }
            #[cfg(unix)]
            (RecvHalf::UdSocket(rh), SendHalf::UdSocket(sh)) => {
                uds_impl::Stream::reunite(rh, sh).map(From::from).map_err(|e| e.convert_halves())
            }
            #[allow(unreachable_patterns)]
            (rh, sh) => Err(ReuniteError { rh, sh }),
        }
    }
}
impl StreamCommon for Stream {
    #[inline]
    fn take_error(&self) -> io::Result<Option<io::Error>> {
        dispatch!(Self: x in self => x.take_error())
    }
    #[inline]
    fn peer_creds(&self) -> io::Result<PeerCreds> { dispatch!(Self: x in self => x.peer_creds()) }
}
multimacro! {
    Stream,
    dispatch_read,
    dispatch_write,
}

mkenum!(
/// Receive half of a Tokio-based local socket stream, obtained by splitting a [`Stream`].
///
/// See the [module-level documentation of local sockets](crate::local_socket) for more details.
"local_socket::tokio::" RecvHalf);
impl r#trait::RecvHalf for RecvHalf {
    type Stream = Stream;
}
multimacro! {
    RecvHalf,
    dispatch_read,
}

mkenum!(
/// Send half of a Tokio-based local socket stream, obtained by splitting a [`Stream`].
///
/// See the [module-level documentation of local sockets](crate::local_socket) for more details.
"local_socket::tokio::" SendHalf);
impl r#trait::SendHalf for SendHalf {
    type Stream = Stream;
}
multimacro! {
    SendHalf,
    dispatch_write,
}

/// [`ReuniteError`](crate::error::ReuniteError) for [`Stream`].
pub type ReuniteError = crate::error::ReuniteError<RecvHalf, SendHalf>;

/// Result type for [`.reunite()`](trait::Stream::reunite) on [`Stream`].
pub type ReuniteResult = r#trait::ReuniteResult<Stream>;
