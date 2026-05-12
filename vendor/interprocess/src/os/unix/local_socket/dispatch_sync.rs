use {
    super::super::uds_local_socket as uds_impl,
    crate::local_socket::{ConnectOptions, Listener, ListenerOptions, Stream},
    std::io,
};

#[inline]
pub fn listen(options: ListenerOptions<'_>) -> io::Result<Listener> {
    options.create_sync_as::<uds_impl::Listener>().map(Listener::from)
}
#[inline]
pub fn connect(options: &ConnectOptions<'_>) -> io::Result<Stream> {
    options.connect_sync_as::<uds_impl::Stream>().map(Stream::from)
}
