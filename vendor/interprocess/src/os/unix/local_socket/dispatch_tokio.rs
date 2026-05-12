use {
    super::super::uds_local_socket::tokio as uds_impl,
    crate::local_socket::{
        tokio::{prelude::*, Listener, Stream},
        ConnectOptions, ListenerOptions,
    },
    std::io,
};

#[inline]
pub fn listen(options: ListenerOptions<'_>) -> io::Result<Listener> {
    options.create_tokio_as::<uds_impl::Listener>().map(Listener::from)
}
#[inline]
pub async fn connect(options: &ConnectOptions<'_>) -> io::Result<Stream> {
    uds_impl::Stream::from_options(options).await.map(Stream::from)
}
