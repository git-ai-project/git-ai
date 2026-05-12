mod create_instance;
mod incoming;
mod options;

use {
    super::{c_wrappers, PipeModeTag, PipeStream, PipeStreamRole, RawPipeStream},
    crate::{os::windows::winprelude::*, poison_error, OrErrno, RawOsErrorExt, LOCK_POISON},
    std::{
        fmt::{self, Debug, Formatter},
        io,
        marker::PhantomData,
        mem::replace,
        ptr,
        sync::{
            atomic::{AtomicBool, Ordering::Relaxed},
            Mutex,
        },
    },
    windows_sys::Win32::{
        Foundation::{
            ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, ERROR_PIPE_NOT_CONNECTED,
        },
        System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe},
    },
};
pub use {incoming::*, options::*};

/// The server for a named pipe, listening for connections to clients and producing pipe streams.
///
/// Note that this type does not correspond to any Win32 object, and is an invention of Interprocess
/// in its entirety.
///
/// The only way to create a `PipeListener` is to use [`PipeListenerOptions`]. See its documentation
/// for more.
///
/// # Examples
///
/// ## Basic server
/// ```no_run
#[cfg_attr(doc, doc = doctest_file::include_doctest!("examples/named_pipe/sync/listener.rs"))]
/// ```
pub struct PipeListener<Rm: PipeModeTag, Sm: PipeModeTag> {
    config: PipeListenerOptions<'static>, // We need the options to create new instances
    nonblocking: AtomicBool,
    // TODO implement a handover mechanism for the case of having an instance limit of 1
    stored_instance: Mutex<OwnedHandle>,
    _phantom: PhantomData<(Rm, Sm)>,
}
impl<Rm: PipeModeTag, Sm: PipeModeTag> PipeListener<Rm, Sm> {
    const STREAM_ROLE: PipeStreamRole = PipeStreamRole::get_for_rm_sm::<Rm, Sm>();

    /// Blocks until a client connects to the named pipe, creating a `Stream` to communicate with
    /// the pipe.
    ///
    /// See `incoming` for an iterator version of this.
    ///
    /// **Neglecting to call this periodically may result in new clients being unable to
    /// connect.** This is because a named pipe client connecting to a server immediately puts the
    /// pipe into a connected state, contrary to the concept of *accepting* clients. If a client
    /// connects to and disconnects from a named pipe without `accept` being called between those
    /// two events, the named pipe instance will contain a dead-on-arrival connection that will
    /// prevent new connections until it is removed by a call to `accept`.
    pub fn accept(&self) -> io::Result<PipeStream<Rm, Sm>> {
        let instance_to_hand_out = {
            let mut stored_instance = self.stored_instance.lock().map_err(poison_error)?;
            // Doesn't actually even need to be atomic to begin with, but it's simpler and more
            // convenient to do this instead. The mutex takes care of ordering.
            let nonblocking = self.nonblocking.load(Relaxed);

            block_on_connect_clearing_empty_conns(stored_instance.as_handle())?;

            let new_instance = self.create_instance(nonblocking)?;
            replace(&mut *stored_instance, new_instance)
        };

        let raw = RawPipeStream::new_server(instance_to_hand_out);

        Ok(PipeStream::new(raw))
    }

    /// Creates an iterator which accepts connections from clients, blocking each time `next()` is
    /// called until one connects.
    #[inline]
    pub fn incoming(&self) -> Incoming<'_, Rm, Sm> { Incoming(self) }

    /// Enables or disables the nonblocking mode for all existing instances of the listener and
    /// future ones. By default, it is disabled.
    ///
    /// This should generally be done during creation, using the
    /// [`nonblocking` field](PipeListenerOptions::nonblocking) of the creation options (unless
    /// there's a good reason not to), which allows making one less system call during creation.
    ///
    /// See the documentation of the aforementioned field for the exact effects of enabling this
    /// mode.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let instance = self.stored_instance.lock().map_err(poison_error)?;
        // Doesn't actually even need to be atomic to begin with, but it's simpler and more
        // convenient to do this instead. The mutex takes care of ordering.
        self.nonblocking.store(nonblocking, Relaxed);
        c_wrappers::set_nonblocking_given_readmode(instance.as_handle(), nonblocking, Rm::MODE)?;
        // Make it clear that the lock survives until this moment.
        drop(instance);
        Ok(())
    }

    /// Creates a listener from a handle and a [`PipeListenerOptions`] table with the assumption
    /// that the handle was created with those options.
    ///
    /// The options are necessary to provide because the listener needs to create new instances of
    /// the named pipe server in `.accept()`.
    pub fn from_handle_and_options(
        handle: OwnedHandle,
        options: PipeListenerOptions<'static>,
    ) -> Self {
        Self {
            nonblocking: AtomicBool::new(options.nonblocking),
            config: options,
            stored_instance: Mutex::new(handle),
            _phantom: PhantomData,
        }
    }

    fn create_instance(&self, nonblocking: bool) -> io::Result<OwnedHandle> {
        self.config.create_instance(false, nonblocking, Self::STREAM_ROLE, Rm::MODE)
    }
}
impl<Rm: PipeModeTag, Sm: PipeModeTag> Debug for PipeListener<Rm, Sm> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("PipeListener")
            .field("config", &self.config)
            .field("instance", &self.stored_instance)
            .field("nonblocking", &self.nonblocking.load(Relaxed))
            .finish()
    }
}

/// The returned handle is owned by the listener until the next call to
/// `.accept()`/`<Incoming as Iterator>::next()`, after which it is owned by the returned stream
/// instead.
///
/// This momentarily locks an internal mutex.
impl<Rm: PipeModeTag, Sm: PipeModeTag> AsRawHandle for PipeListener<Rm, Sm> {
    fn as_raw_handle(&self) -> RawHandle {
        self.stored_instance.lock().expect(LOCK_POISON).as_raw_handle()
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> From<PipeListener<Rm, Sm>> for OwnedHandle {
    fn from(p: PipeListener<Rm, Sm>) -> Self {
        p.stored_instance.into_inner().expect(LOCK_POISON)
    }
}

fn block_on_connect_clearing_empty_conns(handle: BorrowedHandle<'_>) -> io::Result<()> {
    loop {
        match block_on_connect(handle) {
            Err(e) if e.raw_os_error().eeq(ERROR_NO_DATA) => disconnect_if_connected(handle)?,
            r => break r,
        }
    }
}

fn block_on_connect(handle: BorrowedHandle<'_>) -> io::Result<()> {
    unsafe { ConnectNamedPipe(handle.as_raw_handle(), ptr::null_mut()) != 0 }
        .true_val_or_errno(())
        .or_else(thunk_accept_error)
}

fn thunk_accept_error(e: io::Error) -> io::Result<()> {
    if e.raw_os_error().eeq(ERROR_PIPE_CONNECTED) {
        Ok(())
    } else if e.raw_os_error().eeq(ERROR_PIPE_LISTENING) {
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    } else {
        Err(e)
    }
}

fn disconnect_if_connected(handle: BorrowedHandle<'_>) -> io::Result<()> {
    unsafe { DisconnectNamedPipe(handle.as_raw_handle()) != 0 }
        .true_val_or_errno(())
        .or_else(|e| if e.raw_os_error().eeq(ERROR_PIPE_NOT_CONNECTED) { Ok(()) } else { Err(e) })
}
