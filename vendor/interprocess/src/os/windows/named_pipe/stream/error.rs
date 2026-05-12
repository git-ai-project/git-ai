use {
    super::*,
    crate::{error::ConversionError, os::windows::winprelude::*},
    std::{
        fmt::{self, Debug, Display, Formatter},
        io,
    },
};

/// Additional contextual information for conversions from a raw handle to a named pipe stream.
///
/// Not to be confused with the Tokio version.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FromHandleErrorKind {
    /// It wasn't possible to determine whether the pipe handle corresponds to a pipe server or a
    /// pipe client.
    IsServerCheckFailed,
    /// The type being converted into has message semantics, but message boundaries are not
    /// preserved in the pipe.
    NoMessageBoundaries,
}
impl FromHandleErrorKind {
    const fn msg(self) -> &'static str {
        use FromHandleErrorKind::*;
        match self {
            IsServerCheckFailed => "failed to determine if the pipe is server-side or not",
            NoMessageBoundaries => "the pipe does not preserve message boundaries",
        }
    }
}
impl From<FromHandleErrorKind> for io::Error {
    fn from(e: FromHandleErrorKind) -> Self { io::Error::other(e.msg()) }
}
impl Display for FromHandleErrorKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result { f.write_str(self.msg()) }
}

/// Error type for [`TryFrom<OwnedHandle>`](TryFrom) constructors.
///
/// Not to be confused with the Tokio version.
pub type FromHandleError = ConversionError<OwnedHandle, FromHandleErrorKind>;

/// [`ReuniteError`](crate::error::ReuniteError) for sync named pipe streams.
pub type ReuniteError<Rm, Sm> =
    crate::error::ReuniteError<RecvPipeStream<Rm>, SendPipeStream<Sm>>;

/// Result type for [`PipeStream::reunite()`].
pub type ReuniteResult<Rm, Sm> = Result<PipeStream<Rm, Sm>, ReuniteError<Rm, Sm>>;
