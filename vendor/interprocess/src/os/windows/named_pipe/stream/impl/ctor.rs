use {
    super::*,
    crate::{
        os::windows::{named_pipe::WaitTimeout, path_conversion::*},
        spin_with_timeout, ConnectWaitMode, ControlFlowExt as _, OptionExt as _,
        OptionTimeoutExt as _,
    },
    std::{ops::ControlFlow, time::Duration},
    widestring::U16CStr,
    windows_sys::Win32::System::Pipes::PIPE_READMODE_MESSAGE,
};

impl RawPipeStream {
    pub(super) fn new(handle: OwnedHandle, is_server: bool, nfv: NeedsFlushVal) -> Self {
        Self {
            handle: ManuallyDrop::new(handle.into()),
            is_server,
            needs_flush: NeedsFlush::from(nfv),
        }
    }
    pub(crate) fn new_server(handle: OwnedHandle) -> Self {
        Self::new(handle, true, NeedsFlushVal::No)
    }
    fn new_client(handle: OwnedHandle) -> Self { Self::new(handle, false, NeedsFlushVal::No) }
    pub(crate) fn connect(
        path: &U16CStr,
        recv: Option<PipeMode>,
        send: Option<PipeMode>,
        wait_mode: ConnectWaitMode,
    ) -> io::Result<Self> {
        let connect =
            |path: &_| np_wrappers::connect_without_waiting(path, recv, send).break_some();
        let timeout = wait_mode.timeout_or_unsupported(
            "synchronous named pipes do not support the deferred connection wait mode",
        )?;

        let handle = if timeout == Some(Duration::ZERO) {
            connect(path).break_value_pf()
        } else {
            Self::connect_spin_loop(path, connect, timeout)
        }
        .some_or_timeout()?;

        if recv == Some(PipeMode::Messages) {
            np_wrappers::set_np_handle_state(
                handle.as_handle(),
                Some(PIPE_READMODE_MESSAGE),
                None,
                None,
            )?;
        }
        Ok(Self::new_client(handle))
    }

    pub(crate) fn connect_spin_loop(
        path: &U16CStr,
        mut connect: impl FnMut(&U16CStr) -> ControlFlow<io::Result<OwnedHandle>>,
        timeout: Option<Duration>,
    ) -> Option<io::Result<OwnedHandle>> {
        spin_with_timeout(
            &mut connect,
            timeout,
            |connect| connect(path),
            |connect, remain| {
                if let Err(e) = np_wrappers::block_for_server(
                    path,
                    remain
                        .map(WaitTimeout::from_duration_clamped)
                        .unwrap_or(WaitTimeout::FOREVER),
                ) {
                    return ControlFlow::Break(Err(e));
                }
                connect(path)
            },
            |_, _| (),
        )
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> PipeStream<Rm, Sm> {
    /// Connects to the specified named pipe at the specified path (the `\\<hostname>\pipe\` prefix
    /// is not added automatically), blocking until a server instance is dispatched.
    #[inline]
    pub fn connect_by_path<'p>(path: impl ToWtf16<'p>) -> io::Result<Self> {
        Self::connect_by_path_with_wait_mode(path, ConnectWaitMode::Unbounded)
    }

    /// Like `connect_by_path`, but also takes a [wait mode](ConnectWaitMode).
    ///
    /// # Errors
    /// The [unbounded wait mode](ConnectWaitMode::Unbounded) is currently
    /// [not supported](io::ErrorKind::Unsupported).
    #[inline]
    pub fn connect_by_path_with_wait_mode<'p>(
        path: impl ToWtf16<'p>,
        wait_mode: ConnectWaitMode,
    ) -> io::Result<Self> {
        RawPipeStream::connect(
            &path.to_wtf_16().map_err(to_io_error)?,
            Rm::MODE,
            Sm::MODE,
            wait_mode,
        )
        .map(Self::new)
    }

    /// Internal constructor used by the listener. It's a logic error, but not UB, to create the
    /// thing from the wrong kind of thing, but that never ever happens, to the best of my ability.
    pub(crate) fn new(raw: RawPipeStream) -> Self {
        Self { raw: raw.into(), _phantom: PhantomData }
    }
}
