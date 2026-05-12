#![allow(private_bounds)]

use {
    crate::{
        bound_util::{RefRead, RefWrite},
        local_socket::{ConnectOptions, Name, PeerCreds},
        Sealed,
    },
    std::{
        fmt::Debug,
        io::{self, prelude::*},
        time::Duration,
    },
};

/// Local socket stream implementations.
///
/// Types on which this trait is implemented are variants of the
/// [`Stream` enum](super::enum::Stream). In addition, it is implemented on `Stream` itself, which
/// makes it a trait object of sorts. See its documentation for more on the semantics of the methods
/// seen here.
pub trait Stream: Read + RefRead + Write + RefWrite + StreamCommon {
    /// Receive half type returned by [`.split()`](Stream::split).
    type RecvHalf: RecvHalf<Stream = Self>;
    /// Send half type returned by [`.split()`](Stream::split).
    type SendHalf: SendHalf<Stream = Self>;

    /// Connects to a local socket server.
    ///
    /// This is equivalent to `ConnectOptions::new().name(name).connect_sync_as::<Self>()`.
    #[inline]
    fn connect(name: Name<'_>) -> io::Result<Self> {
        ConnectOptions::new().name(name).connect_sync_as::<Self>()
    }

    /// Enables or disables the nonblocking mode for the stream. By default, it is disabled.
    ///
    /// In nonblocking mode, receiving and sending immediately returns with the
    /// [`WouldBlock`](io::ErrorKind::WouldBlock) error in situations when they would normally block
    /// for an uncontrolled amount of time. The specific situations are:
    /// - Receiving is attempted and there is no new data available;
    /// - Sending is attempted and the buffer is full due to the other side not yet having
    ///   received previously sent data.
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;

    /// Sets the receive timeout to the specified value. If set to `None` (the default), reads
    /// will block indefinitely if there is no data.
    fn set_recv_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Sets the send timeout to the specified value. If set to `None` (the default), writes
    /// will block indefinitely if there is no space in the send buffer.
    fn set_send_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;

    /// Splits a stream into a receive half and a send half.
    ///
    /// You probably want to avoid this mechanism for the following reasons:
    /// - Placing a stream in an `Rc` or `Arc` produces identical behavior,
    ///   since `&Stream` implements `Read` and `Write`
    /// - Dropping a half does not shut it down like it does with sockets,
    ///   which may be counterintuitive
    fn split(self) -> (Self::RecvHalf, Self::SendHalf);

    /// Attempts to reunite a receive half with a send half to yield the original stream back,
    /// returning both halves as an error if they belong to different streams (or when using this
    /// method on streams that haven't been split to begin with).
    fn reunite(rh: Self::RecvHalf, sh: Self::SendHalf) -> ReuniteResult<Self>;

    /// Connects to a local socket server using the specified options.
    ///
    /// This method typically shouldn't be called directly – use the creation methods on
    /// `ConnectOptions` (`connect_sync`, `connect_sync_as`) instead.
    fn from_options(options: &ConnectOptions<'_>) -> io::Result<Self>;

    // Do not add methods to this trait that aren't directly tied to non-async streams. A new trait,
    // which should be called StreamExtra or StreamCommon or something along those lines, is to be
    // created for features like impersonation (ones that are instantaneous in nature).
}

/// Functionality common between [the `Stream` trait](Stream) and its async counterparts.
pub trait StreamCommon: Debug + Send + Sync + Sized + Sealed + 'static {
    /// Reads the stored error code from the socket, returning `None` if no error has happened
    /// since the last call to a method that propagates stored errors. Subsequent calls will
    /// return `None` until another error occurs.
    fn take_error(&self) -> io::Result<Option<io::Error>>;
    /// Returns a structure containing a subset of the credentials of the other side of the
    /// connection. The nature of the credentials is OS-specific and using some of them for
    /// making security decisions may be subject to race conditions.
    ///
    /// See the documentation on [`PeerCreds`] for more information.
    ///
    /// ## Platform-specific behavior
    /// ### Unix
    /// The returned credentials are those that were in effect at the time of `connect` (for the
    /// credentials of the client), `listen` (for those of the server). On OpenBSD and NetBSD,
    /// server credentials are instead those which were in effect at the time of `bind` instead.
    fn peer_creds(&self) -> io::Result<PeerCreds>;
}

/// Receive halves of [`Stream`]s, obtained through [`.split()`](Stream::split).
///
/// Types on which this trait is implemented are variants of the
/// [`RecvHalf` enum](super::enum::RecvHalf). In addition, it is implemented on `RecvHalf` itself,
/// which makes it a trait object of sorts.
pub trait RecvHalf: Read + RefRead + Send + Sync + Sized + Sealed + 'static {
    /// The stream type the half is split from.
    type Stream: Stream;

    /// Sets the receive timeout to the specified value. If set to `None` (the default), reads
    /// will block indefinitely if there is no data.
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

/// Send halves of [`Stream`]s, obtained through [`.split()`](Stream::split).
///
/// Types on which this trait is implemented are variants of the
/// [`SendHalf` enum](super::enum::SendHalf). In addition, it is implemented on `SendHalf` itself,
/// which makes it a trait object of sorts.
pub trait SendHalf: Write + RefWrite + Send + Sync + Sized + Sealed + 'static {
    /// The stream type the half is split from.
    type Stream: Stream;

    /// Sets the send timeout to the specified value. If set to `None` (the default), writes
    /// will block indefinitely if there is no space in the send buffer.
    fn set_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

/// [`ReuniteResult`](crate::error::ReuniteResult) for the [`Stream` trait](Stream).
pub type ReuniteResult<S> =
    crate::error::ReuniteResult<S, <S as Stream>::RecvHalf, <S as Stream>::SendHalf>;
