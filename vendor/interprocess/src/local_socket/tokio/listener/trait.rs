use {
    crate::{
        local_socket::{tokio::stream::r#trait::Stream, ListenerOptions},
        Sealed,
    },
    std::{future::Future, io},
};

/// Tokio local socket server implementations.
///
/// Types on which this trait is implemented are variants of the
/// [`Listener` enum](super::enum::Listener). In addition, it is implemented on `Listener` itself,
/// which makes it a trait object of sorts. See its documentation for more on the semantics of the
/// methods seen here.
#[allow(private_bounds)]
pub trait Listener: Send + Sync + Sized + Sealed {
    /// The stream type associated with this listener.
    type Stream: Stream;

    /// Creates a socket server using the specified options.
    fn from_options(options: ListenerOptions<'_>) -> io::Result<Self>;

    /// Asynchronously listens for incoming connections to the socket, returning a future that
    /// finishes only when a client is connected.
    ///
    /// ## Platform-specific behavior
    /// ### Windows
    /// **Neglecting to call this periodically may result in new clients being unable to
    /// connect.** See the warning on [the sync equivalent](crate::local_socket::traits::Listener)
    /// for more information.
    fn accept(&self) -> impl Future<Output = io::Result<Self::Stream>> + Send + Sync;

    /// Disables [name reclamation](super::enum::Listener#name-reclamation) on the listener.
    fn do_not_reclaim_name_on_drop(&mut self);
}
