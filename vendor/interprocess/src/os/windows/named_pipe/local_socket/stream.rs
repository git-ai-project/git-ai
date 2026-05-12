use {
    crate::{
        error::{FromHandleError, ReuniteError},
        local_socket::{
            traits::{self, ReuniteResult},
            ConnectOptions, NameInner, PeerCreds,
        },
        os::windows::{
            local_socket::peer_creds::PeerCreds as PeerCredsInner,
            named_pipe::{pipe_mode::Bytes, DuplexPipeStream, RecvPipeStream, SendPipeStream},
        },
        Sealed,
    },
    std::{
        io::{self, Write},
        os::windows::io::OwnedHandle,
        time::Duration,
    },
};

type StreamImpl = DuplexPipeStream<Bytes>;
type RecvHalfImpl = RecvPipeStream<Bytes>;
type SendHalfImpl = SendPipeStream<Bytes>;

fn no_timeouts() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "named pipes do not support I/O timeouts"))
}

/// Wrapper around [`DuplexPipeStream`] that implements [`Stream`](traits::Stream).
#[derive(Debug)]
pub struct Stream(pub(super) StreamImpl);

impl Sealed for Stream {}
impl traits::Stream for Stream {
    type RecvHalf = RecvHalf;
    type SendHalf = SendHalf;

    fn from_options(options: &ConnectOptions<'_>) -> io::Result<Self> {
        let NameInner::NamedPipe(path) = &options.name.0;
        let stream = StreamImpl::connect_by_path(path.as_ref()).map(Self)?;
        if options.get_nonblocking_stream() {
            stream.set_nonblocking(true)?;
        }
        Ok(stream)
    }

    #[inline]
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.0.set_nonblocking(nonblocking)
    }

    #[inline]
    fn set_recv_timeout(&self, _: Option<Duration>) -> io::Result<()> { no_timeouts() }
    #[inline]
    fn set_send_timeout(&self, _: Option<Duration>) -> io::Result<()> { no_timeouts() }

    #[inline]
    fn split(self) -> (RecvHalf, SendHalf) {
        let (rh, sh) = self.0.split();
        (RecvHalf(rh), SendHalf(sh))
    }
    fn reunite(rh: RecvHalf, sh: SendHalf) -> ReuniteResult<Self> {
        StreamImpl::reunite(rh.0, sh.0).map(Self).map_err(|ReuniteError { rh, sh }| {
            ReuniteError { rh: RecvHalf(rh), sh: SendHalf(sh) }
        })
    }
}

impl traits::StreamCommon for Stream {
    #[inline(always)]
    fn take_error(&self) -> io::Result<Option<io::Error>> { Ok(None) }
    #[inline]
    fn peer_creds(&self) -> io::Result<PeerCreds> {
        Ok(PeerCredsInner { pid: self.0.peer_process_id()? }.into())
    }
}

impl Write for &Stream {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { (&self.0).write(buf) }
    #[inline]
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        (&self.0).write_vectored(bufs)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
    // FUTURE is_write_vectored
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

impl From<Stream> for OwnedHandle {
    fn from(s: Stream) -> Self {
        // The outer local socket interface has receive and send halves and is always duplex in the
        // unsplit type, so a split pipe stream can never appear here.
        s.0.try_into().expect("split named pipe stream inside `local_socket::Stream`")
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
    forward_rbv(StreamImpl, &),
    forward_as_ref(StreamImpl),
    forward_as_mut(StreamImpl),
    forward_sync_read,
    forward_sync_ref_read,
    forward_as_handle,
    forward_try_clone,
    derive_sync_mut_write,
    derive_trivial_conv(StreamImpl),
}

/// Wrapper around [`RecvPipeStream`] that implements [`RecvHalf`](traits::RecvHalf).
pub struct RecvHalf(pub(super) RecvHalfImpl);
impl Sealed for RecvHalf {}
multimacro! {
    RecvHalf,
    forward_rbv(RecvHalfImpl, &),
    forward_as_ref(RecvHalfImpl),
    forward_as_mut(RecvHalfImpl),
    forward_sync_read,
    forward_sync_ref_read,
    forward_as_handle,
    forward_debug("local_socket::RecvHalf"),
    derive_trivial_conv(RecvHalfImpl),
}
impl traits::RecvHalf for RecvHalf {
    type Stream = Stream;

    #[inline]
    fn set_timeout(&self, _: Option<Duration>) -> io::Result<()> { no_timeouts() }
}

/// Wrapper around [`SendPipeStream`] that implements [`SendHalf`](traits::SendHalf).
pub struct SendHalf(pub(super) SendHalfImpl);
impl Sealed for SendHalf {}
multimacro! {
    SendHalf,
    forward_as_ref(SendHalfImpl),
    forward_as_mut(SendHalfImpl),
    forward_as_handle,
    forward_debug("local_socket::SendHalf"),
    derive_sync_mut_write,
    derive_trivial_conv(SendHalfImpl),
}
impl Write for &SendHalf {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { (&self.0).write(buf) }
    #[inline]
    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        (&self.0).write_vectored(bufs)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
    // FUTURE is_write_vectored
}
impl traits::SendHalf for SendHalf {
    type Stream = Stream;

    #[inline]
    fn set_timeout(&self, _: Option<Duration>) -> io::Result<()> { no_timeouts() }
}
