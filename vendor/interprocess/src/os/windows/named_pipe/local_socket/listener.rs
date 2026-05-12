use {
    super::stream::Stream,
    crate::{
        local_socket::{
            traits::{self, Stream as _},
            ListenerNonblockingMode, ListenerOptions, NameInner,
        },
        os::windows::{
            named_pipe::{pipe_mode::Bytes, PipeListener, PipeListenerOptions},
            winprelude::*,
        },
        AtomicEnum, Sealed,
    },
    std::{io, iter::FusedIterator, sync::atomic::Ordering::*},
};

type ListenerImpl = PipeListener<Bytes, Bytes>;

/// Wrapper around [`PipeListener`] that implements [`Listener`](traits::Listener).
#[derive(Debug)]
pub struct Listener {
    listener: ListenerImpl,
    nonblocking: AtomicEnum<ListenerNonblockingMode>,
}
impl Sealed for Listener {}

impl traits::Listener for Listener {
    type Stream = Stream;

    fn from_options(options: ListenerOptions<'_>) -> io::Result<Self> {
        let nb_accept = options.get_nonblocking_accept();
        let nb_stream = options.get_nonblocking_stream();
        let nonblocking = ListenerNonblockingMode::from_bool(nb_accept, nb_stream);

        let mut impl_options = PipeListenerOptions::new();
        let NameInner::NamedPipe(path) = options.name.0;
        impl_options.path = path;
        impl_options.nonblocking = nb_accept;
        impl_options.security_descriptor = options.security_descriptor;

        Ok(Self { listener: impl_options.create()?, nonblocking: AtomicEnum::new(nonblocking) })
    }
    fn accept(&self) -> io::Result<Stream> {
        use ListenerNonblockingMode as LNM;
        let stream = self.listener.accept().map(Stream)?;
        let nonblocking = self.nonblocking.load(Acquire);
        if matches!(nonblocking, LNM::Accept) {
            stream.set_nonblocking(false)?;
        } else if matches!(nonblocking, LNM::Stream) {
            stream.set_nonblocking(true)?;
        }
        Ok(stream)
    }
    fn set_nonblocking(&self, nonblocking: ListenerNonblockingMode) -> io::Result<()> {
        self.listener.set_nonblocking(nonblocking.accept_nonblocking())?;
        self.nonblocking.store(nonblocking, Release);
        Ok(())
    }
    fn do_not_reclaim_name_on_drop(&mut self) {}
}

/// Access to the underlying implementation.
impl Listener {
    /// Borrows the [`PipeListener`] contained within, granting access to operations defined on
    /// it.
    #[inline(always)]
    pub fn inner(&self) -> &ListenerImpl { &self.listener }
    /// Mutably borrows the [`PipeListener`] contained within, granting access to operations
    /// defined on it.
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut ListenerImpl { &mut self.listener }
}

impl Iterator for Listener {
    type Item = io::Result<Stream>;
    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> { Some(traits::Listener::accept(self)) }
}
impl FusedIterator for Listener {}

impl From<Listener> for OwnedHandle {
    #[inline]
    fn from(l: Listener) -> Self { l.listener.into() }
}

impl AsRef<ListenerImpl> for Listener {
    #[inline(always)]
    fn as_ref(&self) -> &ListenerImpl { &self.listener }
}
impl AsMut<ListenerImpl> for Listener {
    #[inline(always)]
    fn as_mut(&mut self) -> &mut ListenerImpl { &mut self.listener }
}
