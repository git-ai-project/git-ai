//! Windows-specific functionality for Tokio-based unnamed pipes.

use {
    crate::{
        os::windows::{unnamed_pipe::CreationOptions, winprelude::*},
        unnamed_pipe::{
            tokio::{Recver as PubRecver, Sender as PubSender},
            Recver as SyncRecver, Sender as SyncSender,
        },
        Sealed, UnpinExt,
    },
    std::{
        io,
        mem::ManuallyDrop,
        pin::Pin,
        task::{ready, Context, Poll},
    },
    tokio::{fs::File, io::AsyncWrite},
};

static INFLIGHT_ERR: &str =
    "cannot deregister unnamed pipe from the Tokio runtime with in-flight operations";

fn pair2pair((tx, rx): (SyncSender, SyncRecver)) -> io::Result<(PubSender, PubRecver)> {
    Ok((PubSender(tx.try_into()?), PubRecver(rx.try_into()?)))
}

#[inline]
pub(crate) fn pipe_impl() -> io::Result<(PubSender, PubRecver)> { pair2pair(super::pipe_impl()?) }

/// Tokio-specific extensions to [`CreationOptions`].
#[allow(private_bounds)]
pub trait CreationOptionsExt: Sealed {
    /// Creates a Tokio-based unnamed pipe and returns its sending and receiving ends, or an error
    /// if one occurred.
    fn create_tokio(self) -> io::Result<(PubSender, PubRecver)>;
}
impl CreationOptionsExt for CreationOptions<'_> {
    #[inline]
    fn create_tokio(self) -> io::Result<(PubSender, PubRecver)> { pair2pair(self.create()?) }
}

#[derive(Debug)]
pub(crate) struct Recver(File);
impl TryFrom<SyncRecver> for Recver {
    type Error = io::Error;
    #[inline]
    fn try_from(rx: SyncRecver) -> io::Result<Self> { Self::try_from(OwnedHandle::from(rx.0)) }
}
impl TryFrom<Recver> for OwnedHandle {
    type Error = io::Error;
    fn try_from(rx: Recver) -> io::Result<Self> {
        rx.0.try_into_std().map(OwnedHandle::from).map_err(|_| io::Error::other(INFLIGHT_ERR))
    }
}
impl TryFrom<OwnedHandle> for Recver {
    type Error = io::Error;
    fn try_from(handle: OwnedHandle) -> io::Result<Self> {
        Ok(Self(File::from_std(handle.into())))
    }
}
multimacro! {
    Recver,
    pinproj_for_unpin(File),
    forward_tokio_read,
    forward_as_handle,
}

#[derive(Debug)]
pub(crate) struct Sender {
    io: ManuallyDrop<File>,
    needs_flush: bool,
}

impl AsyncWrite for Sender {
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.needs_flush = true;
        let rslt = ready!((*self.io).pin().poll_write(cx, buf));
        if rslt.is_err() {
            self.needs_flush = false;
        }
        Poll::Ready(rslt)
    }
    #[inline]
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        // We consider flushing of pipes to not be a thing on all platforms
        Poll::Ready(Ok(()))
    }
    #[inline]
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let h = unsafe { ManuallyDrop::take(&mut self.io) };
        if self.needs_flush {
            linger_pool::linger_boxed(h);
        }
    }
}

impl TryFrom<SyncSender> for Sender {
    type Error = io::Error;
    #[inline]
    fn try_from(rx: SyncSender) -> io::Result<Self> { Self::try_from(OwnedHandle::from(rx.0)) }
}
impl TryFrom<Sender> for OwnedHandle {
    type Error = io::Error;
    fn try_from(mut tx: Sender) -> io::Result<Self> {
        unsafe { ManuallyDrop::take(&mut tx.io) }
            .try_into_std()
            .map(OwnedHandle::from)
            .map_err(|_| io::Error::other(INFLIGHT_ERR))
    }
}
impl TryFrom<OwnedHandle> for Sender {
    type Error = io::Error;
    fn try_from(handle: OwnedHandle) -> io::Result<Self> {
        Ok(Self { io: ManuallyDrop::new(File::from_std(handle.into())), needs_flush: true })
    }
}

impl AsHandle for Sender {
    #[inline]
    fn as_handle(&self) -> BorrowedHandle<'_> { self.io.as_handle() }
}
