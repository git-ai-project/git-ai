use {
    super::*,
    crate::{
        os::windows::{path_conversion::*, NeedsFlushVal},
        ConnectWaitMode, OptionExt as _, OptionTimeoutExt as _,
    },
    std::{borrow::Cow, ops::ControlFlow, time::Duration},
    widestring::U16CStr,
};

impl RawPipeStream {
    pub(super) fn new(inner: InnerTokio, nfv: NeedsFlushVal) -> Self {
        Self {
            inner: ManuallyDrop::new(inner),
            needs_flush: NeedsFlush::from(nfv),
            //recv_msg_state: Mutex::new(RecvMsgState::NotRecving),
        }
    }
    pub(crate) fn new_server(server: TokioNPServer) -> Self {
        Self::new(InnerTokio::Server(server), NeedsFlushVal::No)
    }
    fn new_client(client: TokioNPClient) -> Self {
        Self::new(InnerTokio::Client(client), NeedsFlushVal::No)
    }

    async fn connect(
        path: Cow<'_, U16CStr>,
        recv: Option<PipeMode>,
        send: Option<PipeMode>,
        wait_mode: ConnectWaitMode,
    ) -> io::Result<Self> {
        let connect =
            move |path: &_| c_wrappers::connect_without_waiting(path, recv, send).break_some();
        let timeout = wait_mode.timeout_or_unsupported(
            "Tokio named pipes do not support the deferred connection wait mode",
        )?;

        let client = match connect(&path) {
            ControlFlow::Break(v) => Some(v),
            ControlFlow::Continue(()) if timeout == Some(Duration::ZERO) => None,
            ControlFlow::Continue(()) => {
                let path = path.into_owned();
                tokio::task::spawn_blocking(move || {
                    crate::os::windows::named_pipe::RawPipeStream::connect_spin_loop(
                        &path, connect, timeout,
                    )
                })
                .await
                .map_err(io::Error::other)?
            }
        }
        .some_or_timeout()?;

        // I've double-checked it and it does in fact take ownership of the
        // handle unconditionally. What an egregious footgun, I'm lucky I got
        // it right the first time around.
        let client = unsafe { TokioNPClient::from_raw_handle(client.into_raw_handle())? };

        /* MESSAGE READING DISABLED
        // FIXME(2.4.0) should probably upstream FILE_WRITE_ATTRIBUTES for PipeMode::Messages to Tokio
        if recv == Some(PipeMode::Messages) {
        set_named_pipe_handle_state(client.as_handle(), Some(PIPE_READMODE_MESSAGE), None, None)?;
        }
        */

        Ok(Self::new_client(client))
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> PipeStream<Rm, Sm> {
    /// Connects to the specified named pipe at the specified path (the `\\<hostname>\pipe\` prefix
    /// is not added automatically), waiting until a server instance is dispatched.
    #[inline]
    pub async fn connect_by_path<'s>(path: impl ToWtf16<'s>) -> io::Result<Self> {
        RawPipeStream::connect(
            path.to_wtf_16().map_err(to_io_error)?,
            Rm::MODE,
            Sm::MODE,
            ConnectWaitMode::Unbounded,
        )
        .await
        .map(Self::new)
    }

    /// Like `connect_by_path`, but also takes a [wait mode](ConnectWaitMode).
    ///
    /// # Errors
    /// The [unbounded wait mode](ConnectWaitMode::Unbounded) is currently
    /// [not supported](io::ErrorKind::Unsupported).
    #[inline]
    pub async fn connect_by_path_with_wait_mode<'p>(
        path: impl ToWtf16<'p>,
        wait_mode: ConnectWaitMode,
    ) -> io::Result<Self> {
        RawPipeStream::connect(
            path.to_wtf_16().map_err(to_io_error)?,
            Rm::MODE,
            Sm::MODE,
            wait_mode,
        )
        .await
        .map(Self::new)
    }

    /// Internal constructor used by the listener. It's a logic error, but not UB, to create the
    /// thing from the wrong kind of thing, but that never ever happens, to the best of my ability.
    pub(crate) fn new(raw: RawPipeStream) -> Self {
        Self { raw: raw.into(), flusher: Sm::TokioFlusher::default(), _phantom: PhantomData }
    }
}
