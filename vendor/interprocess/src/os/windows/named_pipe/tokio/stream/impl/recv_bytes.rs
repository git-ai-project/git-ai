use {
    super::*,
    crate::os::windows::downgrade_eof,
    tokio::io::{AsyncRead, ReadBuf},
};

impl RawPipeStream {
    fn poll_read_readbuf(
        &self,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            match downgrade_eof(same_clsrv!(x in self.inner => x.try_read_buf(buf))) {
                Ok(..) => return Poll::Ready(Ok(())),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
            ready!(same_clsrv!(x in self.inner => x.poll_read_ready(cx)))?;
        }
    }
}

impl<Sm: PipeModeTag> AsyncRead for &PipeStream<pipe_mode::Bytes, Sm> {
    #[inline(always)]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.get_mut().raw.get().poll_read_readbuf(cx, buf)
    }
}
impl<Sm: PipeModeTag> AsyncRead for PipeStream<pipe_mode::Bytes, Sm> {
    #[inline(always)]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut &*self), cx, buf)
    }
}
