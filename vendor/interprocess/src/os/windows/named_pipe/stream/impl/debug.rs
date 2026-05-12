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
        if let Some(recv_mode) = recv_mode {
            dbst.field("recv_mode", &recv_mode);
        }
        if let Some(send_mode) = send_mode {
            dbst.field("send_mode", &send_mode);
        }
        dbst.field("handle", &self.handle).field("is_server", &self.is_server)
    }
}

impl<Rm: PipeModeTag, Sm: PipeModeTag> Debug for PipeStream<Rm, Sm> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut dbst = f.debug_struct("PipeStream");
        self.raw.get().fill_fields(&mut dbst, Rm::MODE, Sm::MODE).finish()
    }
}
