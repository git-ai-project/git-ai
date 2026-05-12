use {
    super::{listen_and_maybe_overwrite, ReclaimGuard, Stream},
    crate::{
        local_socket::{traits, ListenerNonblockingMode, ListenerOptions},
        os::unix::c_wrappers,
    },
    std::{
        io,
        iter::FusedIterator,
        os::{
            fd::{AsFd, BorrowedFd, OwnedFd},
            unix::net::UnixListener,
        },
        sync::atomic::{
            AtomicBool,
            Ordering::{Acquire, Release},
        },
    },
};

/// Wrapper around [`UnixListener`] that implements [`Listener`](traits::Listener).
#[derive(Debug)]
pub struct Listener {
    pub(super) listener: UnixListener,
    pub(super) reclaim: ReclaimGuard,
    pub(super) nonblocking_streams: AtomicBool,
}
impl crate::Sealed for Listener {}
impl traits::Listener for Listener {
    type Stream = Stream;

    fn from_options(opts: ListenerOptions<'_>) -> io::Result<Self> {
        let mut reclaim = ReclaimGuard::default();
        let nonblocking_streams = AtomicBool::new(opts.get_nonblocking_stream());
        Ok(Self {
            listener: listen_and_maybe_overwrite(opts, |addr, opts| {
                let rslt = c_wrappers::create_listener(
                    libc::SOCK_STREAM,
                    addr,
                    opts.get_nonblocking_accept(),
                    opts.get_mode(),
                )?;
                reclaim = ReclaimGuard::new(opts.get_reclaim_name(), addr);
                Ok(rslt)
            })
            .map(UnixListener::from)?,
            reclaim,
            nonblocking_streams,
        })
    }
    #[inline]
    fn accept(&self) -> io::Result<Stream> {
        // TODO do our own accept4 and pass SOCK_NONBLOCK on supported platforms
        let stream = self.listener.accept().map(|(s, _)| Stream::from(s))?;
        if self.nonblocking_streams.load(Acquire) {
            c_wrappers::fast_set_nonblocking(stream.as_fd(), true)?;
        }
        Ok(stream)
    }
    #[inline]
    fn set_nonblocking(&self, nonblocking: ListenerNonblockingMode) -> io::Result<()> {
        use ListenerNonblockingMode::*;
        self.listener.set_nonblocking(matches!(nonblocking, Accept | Both))?;
        self.nonblocking_streams.store(matches!(nonblocking, Stream | Both), Release);
        Ok(())
    }
    fn do_not_reclaim_name_on_drop(&mut self) { self.reclaim.forget(); }
}
impl Iterator for Listener {
    type Item = io::Result<Stream>;
    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> { Some(traits::Listener::accept(self)) }
}
impl FusedIterator for Listener {}

/// Unix-specific features.
impl Listener {
    /// Sets whether newly created streams will have the nonblocking flag set by default or not.
    ///
    /// This exists due to a quirk of local socket listener nonblocking mode on Windows.
    pub fn set_new_stream_nonblocking(&self, nonblocking: bool) {
        self.nonblocking_streams.store(nonblocking, Release);
    }
}

/// Access to the underlying implementation.
impl Listener {
    /// Borrows the [`UnixListener`] contained within, granting access to operations defined on it.
    #[inline(always)]
    pub fn inner(&self) -> &UnixListener { &self.listener }
    /// Mutably borrows the [`UnixListener`] contained within, granting access to operations
    /// defined on it.
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut UnixListener { &mut self.listener }
}

/// Has no name reclamation and defaults to blocking mode for resulting streams.
impl From<UnixListener> for Listener {
    fn from(listener: UnixListener) -> Self {
        Self {
            listener,
            reclaim: ReclaimGuard::default(),
            nonblocking_streams: AtomicBool::new(false),
        }
    }
}
impl From<Listener> for UnixListener {
    fn from(mut l: Listener) -> Self {
        l.reclaim.forget();
        l.listener
    }
}

impl AsFd for Listener {
    #[inline]
    fn as_fd(&self) -> BorrowedFd<'_> { self.listener.as_fd() }
}
impl From<Listener> for OwnedFd {
    #[inline]
    fn from(l: Listener) -> Self { UnixListener::from(l).into() }
}
impl From<OwnedFd> for Listener {
    fn from(fd: OwnedFd) -> Self {
        Listener {
            listener: fd.into(),
            reclaim: ReclaimGuard::default(),
            nonblocking_streams: AtomicBool::new(false),
        }
    }
}
