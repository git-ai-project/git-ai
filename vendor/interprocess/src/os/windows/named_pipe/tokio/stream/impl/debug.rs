use {
    super::*,
    std::fmt::{self, Debug, DebugStruct, Formatter},
};

impl RawPipeStream {
    fn fill_fields<'a, 'b, 'c>(
        &self,
        dbst: &'a mut DebugStruct<'b, 'c>,
        recv_mode: Option<PipeMode>,
        send_mode: Option<PipeMode>,
    ) -> &'a mut DebugStruct<'b, 'c> {
        let (tokio_object, is_server) = match &*self.inner {
            InnerTokio::Server(s) => (s as _, true),
            InnerTokio::Client(c) => (c as _, false),
        };
        if let Some(recv_mode) = recv_mode {
            dbst.field("recv_mode", &recv_mode);
        }
        if let Some(send_mode) = send_mode {
            dbst.field("send_mode", &send_mode);
        }
        dbst.field("tokio_object", tokio_object).field("is_server", &is_server)
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> Debug for PipeStream<Rm, Sm> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut dbst = f.debug_struct("PipeStream");
        self.raw.get().fill_fields(&mut dbst, Rm::MODE, Sm::MODE);
        if Sm::MODE.is_some() {
            dbst.field("flusher", &self.flusher);
        }
        dbst.finish()
    }
}
