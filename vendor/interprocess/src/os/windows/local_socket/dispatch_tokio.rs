use {
    super::super::named_pipe::local_socket::tokio as np_impl,
    crate::local_socket::{
        tokio::{prelude::*, Listener, Stream},
        ConnectOptions, ListenerOptions,
    },
    std::io,
};

#[inline]
pub fn listen(options: ListenerOptions<'_>) -> io::Result<Listener> {
    options.create_tokio_as::<np_impl::Listener>().map(Listener::from)
}
#[inline]
pub async fn connect(options: &ConnectOptions<'_>) -> io::Result<Stream> {
    np_impl::Stream::from_options(options).await.map(Stream::from)
}
