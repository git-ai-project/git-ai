#[cfg(feature = "tokio")]
use crate::local_socket::tokio::Stream as TokioStream;
#[cfg(feature = "tokio")]
use std::future::Future;
use {
    crate::{
        local_socket::{traits, Name, Stream},
        ConnectWaitMode, Sealed, TryClone,
    },
    std::{
        fmt::{self, Debug, Formatter},
        io,
        time::Duration,
    },
};

/// Client-side builder for [local socket streams](traits::Stream), including [`Stream`].
pub struct ConnectOptions<'n> {
    pub(crate) name: Name<'n>,
    flags: u8,
    timeout: Duration,
}
impl Sealed for ConnectOptions<'_> {}

const SHFT_NONBLOCKING_STREAM: u8 = 0;
const SHFT_TIMEOUT: u8 = 1;
const SHFT_DEFERRED: u8 = 2;
const ALL_BITS: u8 = (1 << 3) - 1;

const WAITMODE_UNMASK: u8 = ALL_BITS ^ ((1 << SHFT_TIMEOUT) | (1 << SHFT_DEFERRED));

const fn set_bit(flags: u8, pos: u8, val: bool) -> u8 {
    flags & (ALL_BITS ^ (1 << pos)) | ((val as u8) << pos)
}
const fn has_bit(flags: u8, pos: u8) -> bool { flags & (1 << pos) != 0 }

impl TryClone for ConnectOptions<'_> {
    #[inline]
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { name: self.name.clone(), flags: self.flags, timeout: self.timeout })
    }
}

/// Creation and ownership.
impl ConnectOptions<'_> {
    /// Returns a default set of client options.
    #[inline]
    pub fn new() -> Self { Self { name: Name::invalid(), flags: 0, timeout: Duration::ZERO } }
}

/// Option setters.
impl<'n> ConnectOptions<'n> {
    builder_setters! {
        /// Sets the name the client will connect to.
        name: Name<'n>,
    }
    /// Sets the [wait mode](ConnectWaitMode) of the connection operation.
    ///
    /// This defaults to [unbounded waiting](ConnectWaitMode::Unbounded).
    ///
    /// ## Platform-specific behavior
    /// ### Unix
    /// Number of additional `fcntl`s if `SOCK_NONBLOCK` is available:
    /// | wait_mode \ nonblocking_stream | false | true |
    /// |--------------------------------|-------|------|
    /// | Unbounded                      |     0 |    1 |
    /// | Timeout                        |     1 |    0 |
    /// | Deferred                       |     1 |    0 |
    ///
    /// Number of additional `fcntl`s if `SOCK_NONBLOCK` is not available:
    /// | wait_mode \ nonblocking_stream | false | true |
    /// |--------------------------------|-------|------|
    /// | Unbounded                      |     0 |    1 |
    /// | Timeout                        |     2 |    1 |
    /// | Deferred                       |     2 |    1 |
    ///
    /// ### Windows
    /// `WaitMode::Timeout` has no effect in that attempting to connect to an overloaded named
    /// pipe will always return an error as soon as possible. If the named pipe is across the
    /// network, this may entail blocking on network activity for a duration that exceeds the
    /// specified timeout.
    ///
    /// `WaitMode::Deferred` is not supported, and an error will be returned by `connect_*` if it
    /// is used.
    #[must_use = builder_must_use!()]
    #[inline(always)]
    pub fn wait_mode(mut self, wait_mode: ConnectWaitMode) -> Self {
        let flags = self.flags & WAITMODE_UNMASK;
        match wait_mode {
            ConnectWaitMode::Deferred => self.flags = set_bit(flags, SHFT_DEFERRED, true),
            ConnectWaitMode::Timeout(timeout) => {
                self.flags = set_bit(flags, SHFT_TIMEOUT, true);
                self.timeout = timeout;
            }
            ConnectWaitMode::Unbounded => self.flags = flags,
        };
        self
    }
    /// Sets whether the resulting connection is to have its reads and writes be nonblocking or
    /// not.
    ///
    /// This is disabled by default.
    ///
    /// ## Platform-specific behavior
    /// ### Unix
    /// See [`wait_mode`](Self::wait_mode).
    ///
    /// ### Windows
    /// The same as `.set_nonblocking(true)` immediately after creation.
    #[must_use = builder_must_use!()]
    #[inline(always)]
    pub fn nonblocking_stream(mut self, nonblocking: bool) -> Self {
        self.flags = set_bit(self.flags, SHFT_NONBLOCKING_STREAM, nonblocking);
        self
    }
}

/// Option getters.
impl ConnectOptions<'_> {
    pub(crate) fn get_wait_mode(&self) -> ConnectWaitMode {
        if has_bit(self.flags, SHFT_DEFERRED) {
            ConnectWaitMode::Deferred
        } else if has_bit(self.flags, SHFT_TIMEOUT) {
            ConnectWaitMode::Timeout(self.timeout)
        } else {
            ConnectWaitMode::Unbounded
        }
    }
    pub(crate) fn get_nonblocking_stream(&self) -> bool {
        has_bit(self.flags, SHFT_NONBLOCKING_STREAM)
    }
}

/// Stream constructors.
impl ConnectOptions<'_> {
    /// Creates a [`Stream`] by connecting to the specified local socket name.
    ///
    /// On platforms where there are multiple available implementations, this dispatches to the
    /// appropriate implementation based on where the name points to.
    #[inline]
    pub fn connect_sync(&self) -> io::Result<Stream> { self.connect_sync_as::<Stream>() }
    /// Creates the given [type of stream](traits::Stream) by connecting to the specified local
    /// socket name.
    #[inline]
    pub fn connect_sync_as<S: traits::Stream>(&self) -> io::Result<S> { S::from_options(self) }
    /// Creates a Tokio [`Stream`](TokioStream) by connecting to the specified local socket name.
    ///
    /// On platforms where there are multiple available implementations, this dispatches to the
    /// appropriate implementation based on where the name points to.
    #[inline]
    #[cfg(feature = "tokio")]
    // FUTURE remove + '_
    pub fn connect_tokio(
        &self,
    ) -> impl Future<Output = io::Result<TokioStream>> + Send + Sync + '_ {
        self.connect_tokio_as::<TokioStream>()
    }
    /// Creates the given [type of Tokio stream](traits::tokio::Stream) by connecting to the
    /// specified local socket name.
    #[inline]
    #[cfg(feature = "tokio")]
    // FUTURE remove + '_
    pub fn connect_tokio_as<S: traits::tokio::Stream>(
        &self,
    ) -> impl Future<Output = io::Result<S>> + Send + Sync + '_ {
        S::from_options(self)
    }
}

impl Default for ConnectOptions<'_> {
    #[inline]
    fn default() -> Self { Self::new() }
}

impl Debug for ConnectOptions<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("name", &self.name)
            .field("wait_mode", &self.get_wait_mode())
            .field("nonblocking_stream", &self.get_nonblocking_stream())
            .finish()
    }
}
