use {
    crate::{
        error::{FromHandleError, ReuniteError},
        local_socket::{
            traits::{
                tokio::{self as traits, ReuniteResult},
                StreamCommon,
            },
            ConnectOptions, NameInner, PeerCreds,
        },
        os::windows::{
            local_socket::peer_creds::PeerCreds as PeerCredsInner,
            named_pipe::{
                pipe_mode::Bytes,
                tokio::{DuplexPipeStream, RecvPipeStream, SendPipeStream},
            },
            winprelude::*,
        },
        Sealed,
    },
    std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    },
    tokio::io::AsyncWrite,
};

type StreamImpl = DuplexPipeStream<Bytes>;
type RecvHalfImpl = RecvPipeStream<Bytes>;
type SendHalfImpl = SendPipeStream<Bytes>;

/// Wrapper around [`DuplexPipeStream`] that implements the [`Stream`](traits::Stream) trait.
#[derive(Debug)]
pub struct Stream(pub(super) StreamImpl);
impl Sealed for Stream {}

impl traits::Stream for Stream {
    type RecvHalf = RecvHalf;
    type SendHalf = SendHalf;

    #[inline]
    async fn from_options(options: &ConnectOptions<'_>) -> io::Result<Self> {
        let NameInner::NamedPipe(path) = &options.name.0;
        StreamImpl::connect_by_path(path.as_ref()).await.map(Self)
    }
    #[inline]
    fn split(self) -> (RecvHalf, SendHalf) {
        let (r, w) = self.0.split();
        (RecvHalf(r), SendHalf(w))
    }
    #[inline]
    fn reunite(rh: RecvHalf, sh: SendHalf) -> ReuniteResult<Self> {
        StreamImpl::reunite(rh.0, sh.0).map(Self).map_err(|ReuniteError { rh, sh }| {
            ReuniteError { rh: RecvHalf(rh), sh: SendHalf(sh) }
        })
    }
}
impl StreamCommon for Stream {
    #[inline(always)]
    fn take_error(&self) -> io::Result<Option<io::Error>> { Ok(None) }
    #[inline]
    fn peer_creds(&self) -> io::Result<PeerCreds> {
        Ok(PeerCredsInner { pid: self.0.peer_process_id()? }.into())
    }
}

/// Access to the underlying implementation.
impl Stream {
    /// Borrows the [`DuplexPipeStream`] contained within, granting access to operations defined
    /// on it.
    #[inline(always)]
    pub fn inner(&self) -> &StreamImpl { &self.0 }
    /// Mutably borrows the [`DuplexPipeStream`] contained within, granting access to operations
    /// defined on it.
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut StreamImpl { &mut self.0 }
}

impl AsyncWrite for &Stream {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &self.get_mut().0).poll_write(cx, buf)
    }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl TryFrom<OwnedHandle> for Stream {
    type Error = FromHandleError;

    fn try_from(handle: OwnedHandle) -> Result<Self, Self::Error> {
        match StreamImpl::try_from(handle) {
            Ok(s) => Ok(Self(s)),
            Err(e) => Err(FromHandleError {
                details: Default::default(),
                cause: Some(e.details.into()),
                source: e.source,
            }),
        }
    }
}

multimacro! {
    Stream,
    pinproj_for_unpin(StreamImpl),
    forward_rbv(StreamImpl, &),
    forward_as_ref(StreamImpl),
    forward_as_mut(StreamImpl),
    forward_tokio_read,
    forward_tokio_ref_read,
    forward_as_handle,
    derive_tokio_mut_write,
    derive_trivial_conv(StreamImpl),
}

/// Wrapper around [`RecvPipeStream`] that implements [`RecvHalf`](traits::RecvHalf).
pub struct RecvHalf(pub(super) RecvHalfImpl);
impl Sealed for RecvHalf {}
multimacro! {
    RecvHalf,
    pinproj_for_unpin(RecvHalfImpl),
    forward_rbv(RecvHalfImpl, &),
    forward_as_ref(RecvHalfImpl),
    forward_as_mut(RecvHalfImpl),
    forward_tokio_read,
    forward_tokio_ref_read,
    forward_as_handle,
    forward_debug("local_socket::RecvHalf"),
    derive_trivial_conv(RecvHalfImpl),
}
impl traits::RecvHalf for RecvHalf {
    type Stream = Stream;
}

/// Wrapper around [`SendPipeStream`] that implements [`SendHalf`](traits::SendHalf).
pub struct SendHalf(pub(super) SendHalfImpl);
impl Sealed for SendHalf {}
multimacro! {
    SendHalf,
    forward_rbv(SendHalfImpl, &),
    forward_as_ref(SendHalfImpl),
    forward_as_mut(SendHalfImpl),
    forward_as_handle,
    forward_debug("local_socket::SendHalf"),
    derive_tokio_mut_write,
    derive_trivial_conv(SendHalfImpl),
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
        Pin::new(&mut &self.get_mut().0).poll_write(cx, buf)
    }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
