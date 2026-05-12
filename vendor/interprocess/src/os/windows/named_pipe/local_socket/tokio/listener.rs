use {
    super::Stream,
    crate::{
        local_socket::{traits::tokio as traits, ListenerOptions, NameInner},
        os::windows::named_pipe::{
            pipe_mode, tokio::PipeListener as GenericPipeListener, PipeListenerOptions,
        },
        Sealed,
    },
    std::io,
};

type PipeListener = GenericPipeListener<pipe_mode::Bytes, pipe_mode::Bytes>;

/// Wrapper around [`PipeListener`](GenericPipeListener) that implements the
/// [`Listener`](traits::Listener) trait.
#[derive(Debug)]
pub struct Listener(PipeListener);
impl Sealed for Listener {}
impl traits::Listener for Listener {
    type Stream = Stream;

    fn from_options(options: ListenerOptions<'_>) -> io::Result<Self> {
        let mut impl_options = PipeListenerOptions::new();
        let NameInner::NamedPipe(path) = options.name.0;
        impl_options.path = path;
        impl_options.security_descriptor = options.security_descriptor;
        impl_options.create_tokio().map(Self)
    }
    async fn accept(&self) -> io::Result<Stream> {
        let inner = self.0.accept().await?;
        Ok(Stream(inner))
    }
    fn do_not_reclaim_name_on_drop(&mut self) {}
}

/// Access to the underlying implementation.
impl Listener {
    /// Borrows the [`PipeListener`](GenericPipeListener) contained within, granting access to
    /// operations defined on it.
    #[inline(always)]
    pub fn inner(&self) -> &PipeListener { &self.0 }
    /// Mutably borrows the [`PipeListener`](GenericPipeListener) contained within, granting
    /// access to operations defined on it.
    #[inline(always)]
    pub fn inner_mut(&mut self) -> &mut PipeListener { &mut self.0 }
}

impl From<PipeListener> for Listener {
    #[inline(always)]
    fn from(l: PipeListener) -> Self { Self(l) }
}
impl From<Listener> for PipeListener {
    #[inline(always)]
    fn from(l: Listener) -> Self { l.0 }
}

multimacro! {
    Listener,
    forward_as_ref(PipeListener),
    forward_as_mut(PipeListener),
}
