use {
    super::super::named_pipe::local_socket as np_impl,
    crate::local_socket::{prelude::*, ConnectOptions, Listener, ListenerOptions, Stream},
    std::io,
};

#[inline]
pub fn listen(options: ListenerOptions<'_>) -> io::Result<Listener> {
    options.create_sync_as::<np_impl::Listener>().map(Listener::from)
}
#[inline]
pub fn connect(options: &ConnectOptions<'_>) -> io::Result<Stream> {
    np_impl::Stream::from_options(options).map(Stream::from)
}
