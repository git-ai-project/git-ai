use {
    super::*,
    crate::{
        os::windows::{
            named_pipe::PipeMode, security_descriptor::create_security_attributes, winprelude::*,
        },
        ref2ptr, HandleOrErrno,
    },
    std::num::NonZeroU8,
    windows_sys::Win32::{
        Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAG_WRITE_THROUGH,
        },
        System::Pipes::{CreateNamedPipeW, PIPE_NOWAIT, PIPE_REJECT_REMOTE_CLIENTS},
    },
};

impl PipeListenerOptions<'_> {
    pub(super) fn _create(
        &self,
        role: PipeStreamRole,
        recv_mode: Option<PipeMode>,
    ) -> io::Result<(PipeListenerOptions<'static>, OwnedHandle)> {
        let owned_config = self.to_owned()?;

        let instance = self.create_instance(true, self.nonblocking, role, recv_mode)?;
        Ok((owned_config, instance))
    }

    /// Creates an instance of a pipe for a listener with the specified stream type and with the
    /// first-instance flag set to the specified value.
    pub(crate) fn create_instance(
        &self,
        first: bool,
        nonblocking: bool,
        role: PipeStreamRole,
        recv_mode: Option<PipeMode>,
    ) -> io::Result<OwnedHandle> {
        if recv_mode == Some(PipeMode::Messages) && self.mode == PipeMode::Bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "\
cannot create pipe server that has byte type but receives messages – have you forgotten to set \
the `mode` field in `PipeListenerOptions`?",
            ));
        }

        let open_mode = self.open_mode(first, role);
        let pipe_mode = self.pipe_mode(recv_mode, nonblocking);

        let sa = create_security_attributes(
            self.security_descriptor.as_ref().map(|sd| sd.borrow()),
            self.inheritable,
        );

        let max_instances = match self.instance_limit.map(NonZeroU8::get) {
            Some(255) => return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot set 255 as the named pipe instance limit due to 255 being a reserved value",
            )),
            Some(x) => x.into(),
            None => 255,
        };

        unsafe {
            CreateNamedPipeW(
                (*self.path).as_ptr(),
                open_mode,
                pipe_mode,
                max_instances,
                self.output_buffer_size_hint,
                self.input_buffer_size_hint,
                self.wait_timeout.to_raw(),
                ref2ptr(&sa).cast_mut().cast(),
            )
            .handle_or_errno()
            // SAFETY: we just made it and received ownership
            .map(|h| OwnedHandle::from_raw_handle(h))
        }
    }

    fn open_mode(&self, first: bool, role: PipeStreamRole) -> u32 {
        let mut open_mode = FILE_FLAG_OVERLAPPED;
        open_mode |= u32::from(role.direction_as_server());
        if first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        if self.write_through {
            open_mode |= FILE_FLAG_WRITE_THROUGH;
        }
        open_mode
    }
    fn pipe_mode(&self, recv_mode: Option<PipeMode>, nonblocking: bool) -> u32 {
        let mut pipe_mode = 0_u32;
        pipe_mode |= self.mode.to_pipe_type();
        pipe_mode |= recv_mode.map_or(0, PipeMode::to_readmode);
        if nonblocking {
            pipe_mode |= PIPE_NOWAIT;
        }
        if !self.accept_remote {
            pipe_mode |= PIPE_REJECT_REMOTE_CLIENTS;
        }
        pipe_mode
    }
}
