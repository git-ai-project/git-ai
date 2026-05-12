use {
    crate::{
        os::windows::{c_wrappers, winprelude::*, NeedsFlush, OptArcIRC},
        UnpinExt, LOCK_POISON,
    },
    std::{
        future::{self, Future},
        io,
        sync::{atomic::Ordering::*, Mutex},
        task::{ready, Context, Poll},
    },
    tokio::task::JoinHandle,
};

type FlushJH = JoinHandle<io::Result<()>>;

/// Wraps `FlushFileBuffers()` ran in a `spawn_blocking()` task into a poll interface.
#[derive(Debug)]
pub struct TokioFlusher {
    join_handle: Mutex<Option<FlushJH>>,
}
impl TokioFlusher {
    pub(crate) const fn new() -> Self { Self { join_handle: Mutex::new(None) } }
    #[inline]
    pub(crate) async fn flush_atomic(
        &self,
        file_handle: &(impl OptArcIRC<Value = impl AsHandle + Send + Sync + 'static> + 'static),
        needs_flush: &NeedsFlush,
    ) -> io::Result<()> {
        future::poll_fn(|cx| self.poll_flush_atomic(file_handle, needs_flush, cx)).await
    }

    pub(crate) fn poll_flush_atomic(
        &self,
        file_handle: &(impl OptArcIRC<Value = impl AsHandle + Send + Sync + 'static> + 'static),
        needs_flush: &NeedsFlush,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        if !needs_flush.get(Acquire) {
            // Idempotency optimization — don't flush unless there have been unflushed writes
            return Poll::Ready(Ok(()));
        }

        let mut flush = self.join_handle.lock().expect(LOCK_POISON);

        // The mutex is an acquire fence, so this load can safely be relaxed (it can actually be
        // non-atomic in practice, but there's hardly a performance benefit to that)
        if !needs_flush.get(Relaxed) {
            // Lock losering – don't flush if a different thread beat us to the lock
            return Poll::Ready(Ok(()));
        }

        let jh = Self::ensure_flush_start(&mut flush, file_handle);
        let rslt = ready!(jh.pin().poll(cx)).unwrap();
        if rslt.is_ok() {
            needs_flush.clear();
        }
        *flush = None;
        Poll::Ready(rslt)
    }

    pub(crate) fn poll_flush_mut(
        &self,
        file_handle: &(impl OptArcIRC<Value = impl AsHandle + Send + Sync + 'static> + 'static),
        needs_flush: &mut bool,
        cx: &mut Context<'_>,
    ) -> Poll<io::Result<()>> {
        if !*needs_flush {
            // Idempotency optimization – don't flush unless there have been unflushed writes
            return Poll::Ready(Ok(()));
        }

        let mut flush = self.join_handle.lock().expect(LOCK_POISON);

        let jh = Self::ensure_flush_start(&mut flush, file_handle);
        let rslt = ready!(jh.pin().poll(cx)).unwrap();
        if rslt.is_ok() {
            *needs_flush = false;
        }
        *flush = None;
        Poll::Ready(rslt)
    }

    fn ensure_flush_start<'opt>(
        join_handle: &'opt mut Option<FlushJH>,
        file_handle: &(impl OptArcIRC<Value = impl AsHandle + Send + Sync + 'static> + 'static),
    ) -> &'opt mut FlushJH {
        if let Some(jh) = join_handle {
            return jh;
        }
        let fh = file_handle.refclone();
        let task = tokio::task::spawn_blocking(move || c_wrappers::flush(fh.get().as_handle()));
        let ret = join_handle.insert(task);
        ret
    }
}
impl Default for TokioFlusher {
    #[inline]
    fn default() -> Self { Self::new() }
}
