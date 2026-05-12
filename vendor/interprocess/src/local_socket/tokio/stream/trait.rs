#![allow(private_bounds)]

use {
    crate::{
        bound_util::{RefTokioAsyncRead, RefTokioAsyncWrite},
        local_socket::{traits::StreamCommon, ConnectOptions, Name},
        Sealed,
    },
    std::{future::Future, io},
    tokio::io::{AsyncRead, AsyncWrite},
};

/// Tokio local socket stream implementations.
///
/// Types on which this trait is implemented are variants of the
/// [`Stream` enum](super::enum::Stream). In addition, it is implemented on `Stream` itself, which
/// makes it a trait object of sorts. See its documentation for more on the semantics of the methods
/// seen here.
pub trait Stream:
    AsyncRead + RefTokioAsyncRead + AsyncWrite + RefTokioAsyncWrite + StreamCommon
{
    /// Receive half type returned by [`.split()`](Stream::split).
    type RecvHalf: RecvHalf<Stream = Self>;
    /// Send half type returned by [`.split()`](Stream::split).
    type SendHalf: SendHalf<Stream = Self>;

    /// Asynchronously connects to a local socket server.
    ///
    /// This is equivalent to `ConnectOptions::new().name(name).connect_tokio_as::<Self>()`.
    fn connect(name: Name<'_>) -> impl Future<Output = io::Result<Self>> + Send + Sync {
        async { ConnectOptions::new().name(name).connect_tokio_as::<Self>().await }
    }

    /// Splits a stream into a receive half and a send half.
    ///
    /// You probably want to avoid this mechanism for the following reasons:
    /// - Placing a stream in an `Rc` or `Arc` produces identical behavior,
    ///   since `&Stream` implements `Read` and `Write`
    /// - Dropping a half does not shut it down like it does with sockets,
    ///   which may be counterintuitive
    fn split(self) -> (Self::RecvHalf, Self::SendHalf);

    /// Attempts to reunite a receive half with a send half to yield the original stream back,
    /// returning both halves as an error if they belong to different streams.
    fn reunite(rh: Self::RecvHalf, sh: Self::SendHalf) -> ReuniteResult<Self>;

    /// Connects to a local socket server using the specified options.
    ///
    /// This method typically shouldn't be called directly – use the creation methods on
    /// `ConnectOptions` (`connect_tokio`, `connect_tokio_as`) instead.
    // FUTURE add use<Self>
    fn from_options(
        options: &ConnectOptions<'_>,
    ) -> impl Future<Output = io::Result<Self>> + Send + Sync;
}

/// Receive halves of Tokio [`Stream`]s, obtained through [`.split()`](Stream::split).
///
/// Types on which this trait is implemented are variants of the
/// [`RecvHalf` enum](super::enum::RecvHalf). In addition, it is implemented on `RecvHalf` itself,
/// which makes it a trait object of sorts.
pub trait RecvHalf:
    AsyncRead + RefTokioAsyncRead + Send + Sync + Sized + Sealed + 'static
{
    /// The stream type the half is split from.
    type Stream: Stream;
}

/// Send halves of Tokio [`Stream`]s, obtained through [`.split()`](Stream::split).
///
/// Types on which this trait is implemented are variants of the
/// [`SendHalf` enum](super::enum::SendHalf). In addition, it is implemented on `SendHalf` itself,
/// which makes it a trait object of sorts.
pub trait SendHalf:
    AsyncWrite + RefTokioAsyncWrite + Send + Sync + Sized + Sealed + 'static
{
    /// The stream type the half is split from.
    type Stream: Stream;
}

/// [`ReuniteResult`](crate::error::ReuniteResult) for the [Tokio `Stream` trait](Stream).
pub type ReuniteResult<S> =
    crate::error::ReuniteResult<S, <S as Stream>::RecvHalf, <S as Stream>::SendHalf>;
