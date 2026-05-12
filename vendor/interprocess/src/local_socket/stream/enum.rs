#[cfg(unix)]
use crate::os::unix::uds_local_socket as uds_impl;
#[cfg(windows)]
use crate::os::windows::named_pipe::local_socket as np_impl;
use {
    super::r#trait,
    crate::{
        local_socket::{ConnectOptions, PeerCreds},
        TryClone,
    },
    std::{
        io::{self, prelude::*, IoSlice, IoSliceMut},
        time::Duration,
    },
};

impmod! {local_socket::dispatch_sync}

macro_rules! dispatch_read {
    (@iw $ty:ident) => {
        #[inline]
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            dispatch!($ty: x in self => x.read(buf))
        }
        #[inline]
        fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
            dispatch!($ty: x in self => x.read_vectored(bufs))
        }
    };
    ($ty:ident) => {
        impl Read for &$ty {
            dispatch_read!(@iw $ty);
        }
        impl Read for $ty {
            dispatch_read!(@iw $ty);
        }
    };
}
macro_rules! dispatch_write {
    (@iw $ty:ident) => {
        #[inline]
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            dispatch!($ty: x in self => x.write(buf))
        }
        #[inline]
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
        #[inline]
        fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
            dispatch!($ty: x in self => x.write_vectored(bufs))
        }
    };
    ($ty:ident) => {
        /// Flushing is an always successful no-op.
        impl Write for &$ty {
            dispatch_write!(@iw $ty);
        }
        /// Flushing is an always successful no-op.
        impl Write for $ty {
            dispatch_write!(@iw $ty);
        }
    };
}

mkenum!(
/// Local socket byte stream, obtained either from [`Listener`](super::super::Listener) or by
/// connecting to an existing local socket.
///
/// See the [module-level documentation](crate::local_socket) for more details.
///
/// # Examples
///
/// ## Basic client
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/local_socket/sync/stream.rs"))]
/// ```
Stream);

impl r#trait::Stream for Stream {
    type RecvHalf = RecvHalf;
    type SendHalf = SendHalf;

    #[inline]
    fn from_options(options: &ConnectOptions<'_>) -> io::Result<Self> {
        dispatch_sync::connect(options)
    }

    #[inline]
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        dispatch!(Self: x in self => x.set_nonblocking(nonblocking))
    }

    #[inline]
    fn set_recv_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        dispatch!(Self: x in self => x.set_recv_timeout(timeout))
    }
    #[inline]
    fn set_send_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        dispatch!(Self: x in self => x.set_send_timeout(timeout))
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
impl r#trait::StreamCommon for Stream {
    #[inline]
    fn take_error(&self) -> io::Result<Option<io::Error>> {
        dispatch!(Self: x in self => x.take_error())
    }
    #[inline]
    fn peer_creds(&self) -> io::Result<PeerCreds> { dispatch!(Self: x in self => x.peer_creds()) }
}
impl TryClone for Stream {
    fn try_clone(&self) -> io::Result<Self> {
        dispatch!(Self: x in self => x.try_clone()).map(From::from)
    }
}
multimacro! {
    Stream,
    dispatch_read,
    dispatch_write,
}

mkenum!(
/// Receive half of a local socket stream, obtained by splitting a [`Stream`].
///
/// See the [module-level documentation](crate::local_socket) for more details.
"local_socket::" RecvHalf);

impl r#trait::RecvHalf for RecvHalf {
    type Stream = Stream;

    #[inline]
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        dispatch!(Self: x in self => x.set_timeout(timeout))
    }
}
dispatch_read!(RecvHalf);

mkenum!(
/// Send half of a local socket stream, obtained by splitting a [`Stream`].
///
/// See the [module-level documentation](crate::local_socket) for more details.
"local_socket::" SendHalf);

impl r#trait::SendHalf for SendHalf {
    type Stream = Stream;

    #[inline]
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        dispatch!(Self: x in self => x.set_timeout(timeout))
    }
}
dispatch_write!(SendHalf);

/// [`ReuniteError`](crate::error::ReuniteError) for [`Stream`].
pub type ReuniteError = crate::error::ReuniteError<RecvHalf, SendHalf>;

/// Result type for [`.reunite()`](trait::Stream::reunite) on [`Stream`].
pub type ReuniteResult = r#trait::ReuniteResult<Stream>;
