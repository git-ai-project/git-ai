use {
    super::*,
    crate::os::windows::NeedsFlushVal,
    std::mem::ManuallyDrop,
    windows_sys::Win32::System::Pipes::{PIPE_SERVER_END, PIPE_TYPE_MESSAGE},
};

impl AsHandle for InnerTokio {
    #[inline]
    fn as_handle(&self) -> BorrowedHandle<'_> { same_clsrv!(x in self => x.as_handle()) }
}
derive_asraw!(InnerTokio);

impl AsHandle for RawPipeStream {
    #[inline]
    fn as_handle(&self) -> BorrowedHandle<'_> { self.inner.as_handle() }
}
derive_asraw!(RawPipeStream);

impl RawPipeStream {
    fn try_from_handle_given_flags(
        handle: OwnedHandle,
        flags: u32,
    ) -> Result<Self, FromHandleError> {
        let rh = handle.as_raw_handle();
        let handle = ManuallyDrop::new(handle);

        let tkresult = unsafe {
            match flags & PIPE_SERVER_END != 0 {
                true => TokioNPServer::from_raw_handle(rh).map(InnerTokio::Server),
                false => TokioNPClient::from_raw_handle(rh).map(InnerTokio::Client),
            }
        };
        match tkresult {
            Ok(s) => Ok(Self::new(s, NeedsFlushVal::Once)),
            Err(e) => Err(FromHandleError {
                details: FromHandleErrorKind::TokioError,
                cause: Some(e),
                source: Some(ManuallyDrop::into_inner(handle)),
            }),
        }
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
        match c_wrappers::get_flags(handle.as_handle()) {
            Ok(flags) => Self::try_from_handle_given_flags(handle, flags),
            Err(e) => Err(is_server_check_failed_error(e, handle)),
        }
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> AsHandle for PipeStream<Rm, Sm> {
    fn as_handle(&self) -> BorrowedHandle<'_> { self.raw.get().as_handle() }
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

    fn try_from(handle: OwnedHandle) -> Result<Self, Self::Error> {
        let flags = match c_wrappers::get_flags(handle.as_handle()) {
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
        let raw = RawPipeStream::try_from_handle_given_flags(handle, flags)?;
        Ok(Self::new(raw))
    }
}

derive_asraw!({Rm: PipeModeTag, Sm: PipeModeTag} PipeStream<Rm, Sm>, windows);
