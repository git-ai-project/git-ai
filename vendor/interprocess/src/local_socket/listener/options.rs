#[cfg(feature = "tokio")]
use crate::local_socket::tokio::Listener as TokioListener;
#[cfg(windows)]
use crate::os::windows::security_descriptor::SecurityDescriptor;
use {
    crate::{
        local_socket::{traits, Listener, ListenerNonblockingMode, Name},
        Sealed, TryClone,
    },
    std::{
        fmt::{self, Debug, Formatter},
        io,
    },
};

/// Server-side builder for [local socket listeners](traits::Listener), including [`Listener`].
pub struct ListenerOptions<'n> {
    pub(crate) name: Name<'n>,
    flags: u8,
    #[cfg(unix)]
    max_spin_time: std::time::Duration,
    #[cfg(unix)]
    mode: libc::mode_t,
    #[cfg(windows)]
    pub(crate) security_descriptor: Option<SecurityDescriptor>,
}
impl Sealed for ListenerOptions<'_> {}

const SHFT_NONBLOCKING_ACCEPT: u8 = 0;
const SHFT_NONBLOCKING_STREAM: u8 = 1;
const SHFT_RECLAIM_NAME: u8 = 2;
const SHFT_TRY_OVERWRITE: u8 = 3;
const SHFT_HAS_MODE: u8 = 4;
const SHFT_HAS_MAX_SPIN_TIME: u8 = 5;

const ALL_BITS: u8 = (1 << 6) - 1;
const NONBLOCKING_BITS: u8 = (1 << SHFT_NONBLOCKING_ACCEPT) | (1 << SHFT_NONBLOCKING_STREAM);
const fn set_bit(flags: u8, pos: u8, val: bool) -> u8 {
    flags & (ALL_BITS ^ (1 << pos)) | ((val as u8) << pos)
}
const fn has_bit(flags: u8, pos: u8) -> bool { flags & (1 << pos) != 0 }

impl TryClone for ListenerOptions<'_> {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            name: self.name.clone(),
            flags: self.flags,
            #[cfg(unix)]
            max_spin_time: self.max_spin_time,
            #[cfg(unix)]
            mode: self.mode,
            #[cfg(windows)]
            security_descriptor: self
                .security_descriptor
                .as_ref()
                .map(TryClone::try_clone)
                .transpose()?,
        })
    }
}

/// Creation and ownership.
impl ListenerOptions<'_> {
    /// Returns a default set of listener options.
    #[inline]
    pub fn new() -> Self {
        Self {
            name: Name::invalid(),
            flags: 1 << SHFT_RECLAIM_NAME,
            #[cfg(unix)]
            max_spin_time: std::time::Duration::ZERO,
            #[cfg(unix)]
            mode: 0,
            #[cfg(windows)]
            security_descriptor: None,
        }
    }
}

/// Option setters.
impl<'n> ListenerOptions<'n> {
    builder_setters! {
        /// Sets the name the server will listen on.
        name: Name<'n>,
    }
    /// Selects the nonblocking mode to be used by the listener.
    ///
    /// The default value is `Neither`.
    #[must_use = builder_must_use!()]
    #[inline(always)]
    pub fn nonblocking(mut self, nonblocking: ListenerNonblockingMode) -> Self {
        self.flags = (self.flags & (ALL_BITS ^ NONBLOCKING_BITS)) | nonblocking as u8;
        self
    }
    /// Sets whether [name reclamation](Listener#name-reclamation) is to happen or not.
    ///
    /// This is enabled by default.
    #[must_use = builder_must_use!()]
    #[inline(always)]
    pub fn reclaim_name(mut self, reclaim_name: bool) -> Self {
        self.flags = set_bit(self.flags, SHFT_RECLAIM_NAME, reclaim_name);
        self
    }
    /// Sets whether an attempt to handle [`AddrInUse`](std::io::ErrorKind::AddrInUse) errors by
    /// overwriting an existing listener (in the same manner as in
    /// [name reclamation](Listener#name-reclamation)) is to be made or not.
    ///
    /// If this is enabled, name reclamation will be performed on behalf of a previous listener,
    /// even if it is still running and accepting connections, thereby displacing it from the
    /// socket name so that the newly created listener could take its place.
    ///
    /// This is disabled by default.
    ///
    /// ## Platform-specific behavior
    /// ### Unix
    /// On Unix, this deletes the socket file if an `AddrInUse` error is encountered. The previous
    /// listener, if it is still listening on its socket, is not (and in fact cannot be) notified
    /// of this in any way. Note that, by default, this will spin for an unbounded amount of time
    /// if there is contention – set [`max_spin_time`](Self::max_spin_time) if this is
    /// undesirable.
    ///
    /// The deletion suffers from an unavoidable TOCTOU race between the `AddrInUse` error being
    /// observed and the socket file being deleted, since another process may replace the socket
    /// file with a different file, causing Interprocess to delete that file instead. Note that
    /// this generally has no inadvertent privilege escalation implications, as the privileges
    /// required for renaming a file are the same as the ones required for deleting it, but the
    /// behavior may still be surprising in this (admittedly rather artificial) edge case.
    ///
    /// ### Windows
    /// Does nothing (meaning the error goes unhandled), as named pipes cannot be overwritten.
    #[must_use = builder_must_use!()]
    #[inline(always)]
    pub fn try_overwrite(mut self, try_overwrite: bool) -> Self {
        self.flags = set_bit(self.flags, SHFT_TRY_OVERWRITE, try_overwrite);
        self
    }
    /// Sets the maximum cumulative amount of time for which listener creation may spin in retry
    /// loops.
    ///
    /// Spinning under contention is, of course, avoided whenever possible, but certain features
    /// simply can't be implemented without retry loops that have to loop for up to however long
    /// contention persists. If you'd like to limit the duration for which listener creation can
    /// be delayed by other processes, including potentially untrusted ones, set this option to
    /// a value that you consider reasonably low.
    ///
    /// ## Platform-specific behavior
    /// ### Unix
    /// Made use of in the implementation of [`try_overwrite`](Self::try_overwrite).
    ///
    /// ### Windows
    /// Currently not used for anything.
    #[must_use = builder_must_use!()]
    #[inline(always)]
    #[cfg_attr(not(unix), allow(unused_mut))]
    pub fn max_spin_time(mut self, max_spin_time: std::time::Duration) -> Self {
        #[cfg(unix)]
        {
            self.flags |= 1 << SHFT_HAS_MAX_SPIN_TIME;
            self.max_spin_time = max_spin_time;
        }
        let _ = max_spin_time;
        self
    }
    #[cfg(unix)]
    #[inline(always)]
    pub(crate) fn set_mode(&mut self, mode: libc::mode_t) {
        self.flags |= 1 << SHFT_HAS_MODE;
        self.mode = mode;
    }
}

/// Option getters.
impl ListenerOptions<'_> {
    pub(crate) fn get_nonblocking_accept(&self) -> bool {
        has_bit(self.flags, SHFT_NONBLOCKING_ACCEPT)
    }
    pub(crate) fn get_nonblocking_stream(&self) -> bool {
        has_bit(self.flags, SHFT_NONBLOCKING_STREAM)
    }
    pub(crate) fn get_reclaim_name(&self) -> bool { has_bit(self.flags, SHFT_RECLAIM_NAME) }
    pub(crate) fn get_try_overwrite(&self) -> bool { has_bit(self.flags, SHFT_TRY_OVERWRITE) }
    #[cfg(unix)]
    pub(crate) fn get_mode(&self) -> Option<libc::mode_t> {
        has_bit(self.flags, SHFT_HAS_MODE).then_some(self.mode)
    }
    #[cfg(unix)]
    pub(crate) fn get_max_spin_time(&self) -> Option<std::time::Duration> {
        has_bit(self.flags, SHFT_HAS_MAX_SPIN_TIME).then_some(self.max_spin_time)
    }
    #[cfg(unix)]
    pub(crate) fn get_max_spin_time_mut(&mut self) -> Option<&mut std::time::Duration> {
        has_bit(self.flags, SHFT_HAS_MAX_SPIN_TIME).then_some(&mut self.max_spin_time)
    }
}

/// Listener constructors.
impl ListenerOptions<'_> {
    /// Creates a [`Listener`], binding it to the specified local socket name.
    ///
    /// On platforms where there are multiple available implementations, this dispatches to the
    /// appropriate implementation based on where the name points to.
    #[inline]
    pub fn create_sync(self) -> io::Result<Listener> { self.create_sync_as::<Listener>() }
    /// Creates the given [type of listener](traits::Listener), binding it to the specified local
    /// socket name.
    #[inline]
    pub fn create_sync_as<L: traits::Listener>(self) -> io::Result<L> { L::from_options(self) }
    /// Creates a Tokio [`Listener`](TokioListener), binding it to the specified local socket
    /// name.
    ///
    /// On platforms where there are multiple available implementations, this dispatches to the
    /// appropriate implementation based on where the name points to.
    #[inline]
    #[cfg(feature = "tokio")]
    pub fn create_tokio(self) -> io::Result<TokioListener> {
        self.create_tokio_as::<TokioListener>()
    }
    /// Creates the given [type of listener](traits::tokio::Listener), binding it to the specified
    /// local socket name.
    #[inline]
    #[cfg(feature = "tokio")]
    pub fn create_tokio_as<L: traits::tokio::Listener>(self) -> io::Result<L> {
        L::from_options(self)
    }
}

impl Default for ListenerOptions<'_> {
    #[inline]
    fn default() -> Self { Self::new() }
}

impl Debug for ListenerOptions<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut dbs = f.debug_struct("ListenerOptions");
        let nonblocking = ListenerNonblockingMode::from_bool(
            self.get_nonblocking_accept(),
            self.get_nonblocking_stream(),
        );
        dbs.field("name", &self.name)
            .field("nonblocking", &nonblocking)
            .field("reclaim_name", &self.get_reclaim_name())
            .field("try_overwrite", &self.get_try_overwrite());
        #[cfg(unix)]
        {
            dbs.field("max_spin_time", &self.get_max_spin_time());
        }
        #[cfg(unix)]
        {
            // FIXME not octal
            dbs.field("mode", &self.get_mode());
        }
        #[cfg(windows)]
        {
            dbs.field("security_descriptor", &self.security_descriptor);
        }
        dbs.finish()
    }
}
