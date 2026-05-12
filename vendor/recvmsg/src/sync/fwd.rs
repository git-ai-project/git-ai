use super::*;
use alloc::boxed::Box;

impl<T: TruncatingRecvMsg + ?Sized> TruncatingRecvMsg for &mut T {
    type Error = T::Error;
    type AddrBuf = T::AddrBuf;
    forward_trait_methods! {
        fn recv_trunc(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut Self::AddrBuf>,
        ) -> Result<Option<bool>, Self::Error>;
        fn discard_msg(&mut self) -> Result<(), Self::Error>;
    }
}
impl<T: TruncatingRecvMsg + ?Sized> TruncatingRecvMsg for Box<T> {
    type Error = T::Error;
    type AddrBuf = T::AddrBuf;
    forward_trait_methods! {
        fn recv_trunc(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut Self::AddrBuf>,
        ) -> Result<Option<bool>, Self::Error>;
        fn discard_msg(&mut self) -> Result<(), Self::Error>;
    }
}

impl<T: TruncatingRecvMsgWithFullSize + ?Sized> TruncatingRecvMsgWithFullSize for &mut T {
    forward_trait_methods! {
        fn recv_trunc_with_full_size(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut Self::AddrBuf>,
        ) -> Result<TryRecvResult, Self::Error>;
    }
}
impl<T: TruncatingRecvMsgWithFullSize + ?Sized> TruncatingRecvMsgWithFullSize for Box<T> {
    forward_trait_methods! {
        fn recv_trunc_with_full_size(
            &mut self,
            peek: bool,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut Self::AddrBuf>,
        ) -> Result<TryRecvResult, Self::Error>;
    }
}

impl<T: RecvMsg + ?Sized> RecvMsg for &mut T {
    type Error = T::Error;
    type AddrBuf = T::AddrBuf;
    forward_trait_methods! {
        fn recv_msg(
            &mut self,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut Self::AddrBuf>,
        ) -> Result<RecvResult, Self::Error>;
    }
}
impl<T: RecvMsg + ?Sized> RecvMsg for alloc::boxed::Box<T> {
    type Error = T::Error;
    type AddrBuf = T::AddrBuf;
    forward_trait_methods! {
        fn recv_msg(
            &mut self,
            buf: &mut MsgBuf<'_>,
            abuf: Option<&mut Self::AddrBuf>,
        ) -> Result<RecvResult, Self::Error>;
    }
}
