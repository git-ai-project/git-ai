use {
    super::*,
    crate::{os::windows::named_pipe::PmtNotNone, UnpinExt},
    tokio::io::AsyncWrite,
};

impl RawPipeStream {
    fn poll_write(&self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        loop {
            ready!(same_clsrv!(x in self.inner => x.poll_write_ready(cx)))?;
            match same_clsrv!(x in self.inner => x.try_write(buf)) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                els => {
                    self.needs_flush.mark_dirty();
                    return Poll::Ready(els);
                }
            }
        }
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag + PmtNotNone> PipeStream<Rm, Sm> {
    /// Flushes the stream, waiting until the send buffer is empty (has been received by the other
    /// end in its entirety).
    ///
    /// Only available on streams that have a send mode.
    #[inline]
    pub async fn flush(&self) -> io::Result<()> {
        self.flusher.flush_atomic(Sm::cast_oai(&self.raw), &self.raw.get().needs_flush).await
    }

    /// Polls the future of `.flush()`.
    #[inline]
    pub fn poll_flush(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flusher.poll_flush_atomic(Sm::cast_oai(&self.raw), &self.raw.get().needs_flush, cx)
    }

    /// Marks the stream as unflushed, preventing elision of the next flush operation (which
    /// includes limbo).
    #[inline]
    pub fn mark_dirty(&self) { self.raw.get().needs_flush.mark_dirty(); }
    /// Assumes that the other side has consumed everything that's been written so far. This will
    /// turn the next flush into a no-op, but will cause the send buffer to be cleared when the
    /// stream is closed, since it won't be sent to limbo.
    ///
    /// If there's already an outstanding `.flush()` operation, it won't be affected by this call.
    #[inline]
    pub fn assume_flushed(&self) { self.raw.get().needs_flush.take(); }
    /// Drops the stream without sending it to limbo. This is the same as calling `assume_flushed()`
    /// right before dropping it.
    ///
    /// If there's already an outstanding `.flush()` operation, it won't be affected by this call.
    #[inline]
    pub fn evade_limbo(self) { self.assume_flushed(); }
}

impl<Rm: PipeModeTag> PipeStream<Rm, pipe_mode::Messages> {
    /// Sends a message into the pipe, returning how many bytes were successfully sent (typically
    /// equal to the size of what was requested to be sent).
    #[inline]
    pub async fn send(&self, buf: &[u8]) -> io::Result<usize> {
        struct Write<'a>(&'a RawPipeStream, &'a [u8]);
        impl Future for Write<'_> {
            type Output = io::Result<usize>;
            #[inline]
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let slf = self.get_mut();
                slf.0.poll_write(cx, slf.1)
            }
        }
        Write(self.raw.get(), buf).await
    }
}

impl<Rm: PipeModeTag> AsyncWrite for &PipeStream<Rm, pipe_mode::Bytes> {
    #[inline(always)]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.get_mut().raw.get().poll_write(cx, buf)
    }
    #[inline(always)]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.get_mut().poll_flush(cx)
    }
    #[inline(always)]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_flush(self, cx)
    }
}
impl<Rm: PipeModeTag> AsyncWrite for PipeStream<Rm, pipe_mode::Bytes> {
    #[inline]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        AsyncWrite::poll_write((&mut &*self).pin(), cx, buf)
    }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_flush((&mut &*self).pin(), cx)
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        AsyncWrite::poll_shutdown((&mut &*self).pin(), cx)
    }
}
