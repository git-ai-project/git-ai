use {
    super::*,
    crate::{os::windows::c_wrappers::duplicate_handle, TryClone},
    std::mem::ManuallyDrop,
    windows_sys::Win32::System::Pipes::{PIPE_SERVER_END, PIPE_TYPE_MESSAGE},
};

impl AsHandle for RawPipeStream {
    #[inline]
    fn as_handle(&self) -> BorrowedHandle<'_> { self.handle.as_handle() }
}
derive_asraw!(RawPipeStream);

impl RawPipeStream {
    fn from_handle_given_flags(handle: OwnedHandle, flags: u32) -> Self {
        Self::new(handle, flags & PIPE_SERVER_END != 0, NeedsFlushVal::Once)
    }
}

fn is_server_check_failed_error(cause: io::Error, handle: OwnedHandle) -> FromHandleError {
    FromHandleError {
        details: FromHandleErrorKind::IsServerCheckFailed,
        cause: Some(cause),
        source: Some(handle),
    }
}

impl TryFrom<OwnedHandle> for RawPipeStream {
    type Error = FromHandleError;

    fn try_from(handle: OwnedHandle) -> Result<Self, Self::Error> {
        let flags = match np_wrappers::get_flags(handle.as_handle()) {
            Ok(f) => f,
            Err(e) => return Err(is_server_check_failed_error(e, handle)),
        };
        Ok(Self::from_handle_given_flags(handle, flags))
    }
}
impl From<RawPipeStream> for OwnedHandle {
    #[inline]
    fn from(x: RawPipeStream) -> Self {
        let x = ManuallyDrop::new(x);
        let handle = unsafe { std::ptr::read(&x.handle) };
        ManuallyDrop::into_inner(handle).into()
    }
}

/// Attempts to unwrap the given stream into the raw owned handle type, returning itself back if
/// no ownership over it is available, as is the case when the stream is split.
impl<Rm: PipeModeTag, Sm: PipeModeTag> TryFrom<PipeStream<Rm, Sm>> for OwnedHandle {
    type Error = PipeStream<Rm, Sm>;
    #[inline]
    fn try_from(s: PipeStream<Rm, Sm>) -> Result<Self, Self::Error> {
        match s.raw {
            MaybeArc::Inline(x) => Ok(x.into()),
            MaybeArc::Shared(..) => Err(s),
        }
    }
}

/// Attempts to wrap the given handle into the high-level pipe stream type. If the underlying pipe
/// type is wrong or trying to figure out whether it's wrong or not caused a system call error, the
/// corresponding error condition is returned.
///
/// For more on why this can fail, see [`FromHandleError`]. Most notably, server-side send-only
/// pipes will cause "access denied" errors because they lack permissions to check whether it's a
/// server-side pipe and whether it has message boundaries.
impl<Rm: PipeModeTag, Sm: PipeModeTag> TryFrom<OwnedHandle> for PipeStream<Rm, Sm> {
    type Error = FromHandleError;
    fn try_from(mut handle: OwnedHandle) -> Result<Self, Self::Error> {
        let flags = match np_wrappers::get_flags(handle.as_handle()) {
            Ok(f) => f,
            Err(e) => return Err(is_server_check_failed_error(e, handle)),
        };
        // If the wrapper type tries to receive incoming data as messages, that might break if
        // the underlying pipe has no message boundaries. Let's check for that.
        if Rm::MODE == Some(PipeMode::Messages) && flags & PIPE_TYPE_MESSAGE == 0 {
            return Err(FromHandleError {
                details: FromHandleErrorKind::NoMessageBoundaries,
                cause: None,
                source: Some(handle),
            });
        }
        // FUTURE pass this unlikely error up to the caller
        if let Ok(h) = np_wrappers::reopen_overlapped(handle.as_handle(), Rm::MODE, Sm::MODE) {
            handle = h;
        }
        Ok(Self::new(RawPipeStream::from_handle_given_flags(handle, flags)))
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> TryClone for PipeStream<Rm, Sm> {
    fn try_clone(&self) -> io::Result<Self> {
        let handle = duplicate_handle(self.as_handle())?;
        self.raw.get().needs_flush.on_clone();
        let new = RawPipeStream::new(handle, self.is_server(), NeedsFlushVal::Always);
        Ok(Self::new(new))
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> AsHandle for PipeStream<Rm, Sm> {
    #[inline]
    fn as_handle(&self) -> BorrowedHandle<'_> { self.raw.get().as_handle() }
}

derive_asraw!({Rm: PipeModeTag, Sm: PipeModeTag} PipeStream<Rm, Sm>, windows);
