//! Module used to quickly import all the traits of the crate.
pub use crate::{
    msgbuf::MsgBuf,
    r#async::{
        RecvMsg as AsyncRecvMsg, RecvMsgExt as AsyncRecvMsgExt,
        TruncatingRecvMsg as AsyncTruncatingRecvMsg,
        TruncatingRecvMsgExt as AsyncTruncatingRecvMsgExt,
        TruncatingRecvMsgWithFullSize as AsyncTruncatingRecvMsgWithFullSize,
        TruncatingRecvMsgWithFullSizeExt as AsyncTruncatingRecvMsgWithFullSizeExt,
    },
    sync::{
        RecvMsg, TruncatingRecvMsg, TruncatingRecvMsgWithFullSize, TruncatingRecvMsgWithFullSizeExt,
    },
};
