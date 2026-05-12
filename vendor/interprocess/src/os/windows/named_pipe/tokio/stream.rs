// TODO message reading disabled due to a lack of support in Mio; we should try to figure
// something out, they need to add first-class message pipe support and handling of
// ERROR_MORE_DATA

mod error;
pub use error::*;

mod r#impl;

use {
    crate::os::windows::{
        named_pipe::stream::{pipe_mode, PipeModeTag},
        winprelude::*,
        MaybeArc, NeedsFlush,
    },
    std::{io, marker::PhantomData, mem::ManuallyDrop},
    tokio::net::windows::named_pipe::{
        NamedPipeClient as TokioNPClient, NamedPipeServer as TokioNPServer,
    },
};

/// Tokio-based named pipe stream, created by a server-side listener or by connecting to a server.
///
/// This type combines in itself all possible combinations of receive modes and send modes, plugged
/// into it using the `Rm` and `Sm` generic parameters respectively.
///
/// Pipe streams can be split by reference and by value for concurrent receive and send operations.
/// Splitting by reference is ephemeral and can be achieved by simply borrowing the stream, since
/// both `PipeStream` and `&PipeStream` implement the I/O traits. Splitting by value is done using
/// the [`.split()`](Self::split) method, producing a receive half and a send half, and can be
/// reverted via [`.reunite()`](Self::reunite).
///
/// # Examples
///
/// ## Basic bytestream client
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/named_pipe/sync/stream/bytes.rs"))]
/// ```
pub struct PipeStream<Rm: PipeModeTag, Sm: PipeModeTag> {
    raw: Sm::OptArc<RawPipeStream>,
    // This specializes to TokioFlusher for non-None send modes and to () for receive-only
    // streams, reducing the size of read halves.
    flusher: Sm::TokioFlusher,
    _phantom: PhantomData<(Rm, Sm)>,
}

/// Type alias for a Tokio-based pipe stream with the same receive mode and send mode.
pub type DuplexPipeStream<M> = PipeStream<M, M>;

/// Type alias for a pipe stream with a receive mode but no send mode.
///
/// This can be produced by the listener, by connecting, or by splitting.
pub type RecvPipeStream<M> = PipeStream<M, pipe_mode::None>;
/// Type alias for a pipe stream with a send mode but no receive mode.
///
/// This can be produced by the listener, by connecting, or by splitting.
pub type SendPipeStream<M> = PipeStream<pipe_mode::None, M>;

pub(crate) struct RawPipeStream {
    inner: ManuallyDrop<InnerTokio>,
    // Cleared by the generic pipes rather than by the raw pipe stream, unlike in sync land.
    needs_flush: NeedsFlush,
    // MESSAGE READING DISABLED
    //recv_msg_state: Mutex<RecvMsgState>,
}
enum InnerTokio {
    Server(TokioNPServer),
    Client(TokioNPClient),
}
impl Drop for RawPipeStream {
    fn drop(&mut self) {
        let i = unsafe { ManuallyDrop::take(&mut self.inner) };
        if self.needs_flush.get_mut() {
            match i {
                InnerTokio::Server(p) => linger_pool::linger_boxed(p),
                InnerTokio::Client(p) => linger_pool::linger_boxed(p),
            }
        }
    }
}

/* MESSAGE READING DISABLED
#[derive(Debug, Default)]
#[repr(u8)]
enum RecvMsgState {
    #[default]
    NotRecving,
    Looping {
        spilled: bool,
        partial: bool,
    },
    Discarding {
        result: io::Result<RecvResult>,
    },
}
unsafe impl ReprU8 for RecvMsgState {}
*/
