// MESSAGE READING DISABLED

use {super::*, std::mem::MaybeUninit};

impl RawPipeStream {
    fn poll_read_uninit(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [MaybeUninit<u8>],
    ) -> Poll<io::Result<usize>> {
        let mut readbuf = ReadBuf::uninit(buf);
        ready!(self.poll_read_readbuf(cx, &mut readbuf).map(downgrade_eof))?;
        Poll::Ready(Ok(readbuf.filled().len()))
    }

    fn poll_discard_msg(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut buf = [MaybeUninit::uninit(); DISCARD_BUF_SIZE];
        Poll::Ready(loop {
            match decode_eof(ready!(self.poll_read_uninit(cx, &mut buf))) {
                Ok(..) => break Ok(()),
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break Ok(()),
                Err(e) if e.raw_os_error() == Some(ERROR_MORE_DATA as _) => {}
                Err(e) => break Err(e),
            }
        })
    }

    // TODO clarify in recvmsg that using different buffers across different polls of this function
    // that return Pending makes for unexpected behavior
    fn poll_recv_msg(
        &self,
        cx: &mut Context<'_>,
        buf: &mut MsgBuf<'_>,
        lock: Option<MutexGuard<'_, RecvMsgState>>,
    ) -> Poll<io::Result<RecvResult>> {
        let mut mode = 0;
        match decode_eof(get_named_pipe_handle_state(
            self.as_handle(),
            Some(&mut mode),
            None,
            None,
            None,
            None,
        )) {
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                return Poll::Ready(Ok(RecvResult::EndOfStream))
            }
            els => els,
        }?;
        eprintln!("DBG mode {:#x}", mode);
        let mut state = lock.unwrap_or_else(|| self.recv_msg_state.lock().unwrap());

        match &mut *state {
            RecvMsgState::NotRecving => {
                buf.set_fill(0);
                buf.has_msg = false;
                *state = RecvMsgState::Looping { spilled: false, partial: false };
                self.poll_recv_msg(cx, buf, Some(state))
            }
            RecvMsgState::Looping { spilled, partial } => {
                let mut more_data = true;
                while more_data {
                    let slice = buf.unfilled_part();
                    if slice.is_empty() {
                        match buf.grow() {
                            Ok(()) => {
                                *spilled = true;
                                debug_assert!(!buf.unfilled_part().is_empty());
                            }
                            Err(e) => {
                                let qer = Ok(RecvResult::QuotaExceeded(e));
                                if more_data {
                                    // A partially successful partial receive must result in the
                                    // rest of the message being discarded.
                                    *state = RecvMsgState::Discarding { result: qer };
                                    return self.poll_recv_msg(cx, buf, Some(state));
                                } else {
                                    *state = RecvMsgState::NotRecving;
                                    return Poll::Ready(qer);
                                }
                            }
                        }
                        continue;
                    }

                    let mut rslt = ready!(self.poll_read_uninit(cx, slice));
                    more_data = false;

                    if matches!(&rslt, Ok(0)) {
                        // FIXME(2.4.0) Mio sometimes does broken pipe thunking (this is a bug that
                        // breaks zero-sized messages)
                        rslt = Err(io::Error::from(io::ErrorKind::BrokenPipe));
                    }
                    let incr = match decode_eof(rslt) {
                        Ok(incr) => incr,
                        Err(e) if e.raw_os_error() == Some(ERROR_MORE_DATA as _) => {
                            more_data = true;
                            *partial = true;
                            slice.len()
                        }
                        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                            buf.set_fill(0);
                            return Poll::Ready(Ok(RecvResult::EndOfStream));
                        }
                        Err(e) => {
                            return if *partial {
                                // This is irrelevant to normal operation of downstream
                                // programs, but still makes them easier to debug.
                                *state = RecvMsgState::Discarding { result: Err(e) };
                                self.poll_recv_msg(cx, buf, Some(state))
                            } else {
                                Poll::Ready(Err(e))
                            };
                        }
                    };
                    unsafe {
                        // SAFETY: this one is on Tokio
                        buf.advance_init_and_set_fill(buf.len_filled() + incr)
                    };
                }

                let ret = if *spilled { RecvResult::Spilled } else { RecvResult::Fit };
                *state = RecvMsgState::NotRecving;
                Poll::Ready(Ok(ret))
            }
            RecvMsgState::Discarding { result } => {
                let _ = ready!(self.poll_discard_msg(cx));
                let r = replace(result, Ok(RecvResult::EndOfStream)); // Silly little sentinel...
                *state = RecvMsgState::NotRecving; // ...gone, so very young.
                Poll::Ready(r)
            }
        }
    }
}

impl<Sm: PipeModeTag> AsyncRecvMsg for &PipeStream<pipe_mode::Messages, Sm> {
    type Error = io::Error;
    type AddrBuf = NoAddrBuf;
    #[inline]
    fn poll_recv_msg(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut MsgBuf<'_>,
        _: Option<&mut NoAddrBuf>,
    ) -> Poll<io::Result<RecvResult>> {
        self.raw.poll_recv_msg(cx, buf, None)
    }
}
impl<Sm: PipeModeTag> AsyncRecvMsg for PipeStream<pipe_mode::Messages, Sm> {
    type Error = io::Error;
    type AddrBuf = NoAddrBuf;
    #[inline]
    fn poll_recv_msg(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut MsgBuf<'_>,
        _: Option<&mut NoAddrBuf>,
    ) -> Poll<io::Result<RecvResult>> {
        AsyncRecvMsg::poll_recv_msg((&mut &*self).pin(), cx, buf, None)
    }
}
