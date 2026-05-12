use {
    super::{c_wrappers, winprelude::*},
    std::{
        collections::VecDeque,
        io,
        mem::{align_of, ManuallyDrop},
        ops::Deref,
        sync::{
            atomic::{AtomicBool, Ordering::*},
            Arc, Condvar, Mutex, PoisonError,
        },
        thread::Thread,
        time::{Duration, Instant},
    },
};

static HAS_PERSISTENT_THREAD: AtomicBool = AtomicBool::new(false);
static QUEUE: Queue = Queue::new();

/// Sends the given handle owner off to the linger pool without a heap indirection.
pub fn linger<T: Into<OwnedHandle>>(h: T) {
    let h = HandleFini(h.into());
    linger_ent(QueueEnt::Handle(h))
}
/// Sends the given handle owner off to the linger pool.
///
/// If `T` implements `Into<OwnedHandle>`, use `linger` instead.
pub fn linger_boxed<T: AsHandle + Send + Sync>(ih: T) {
    linger_ent(QueueEnt::IndirectHandle(DynHandleOwner::boxed(ih)))
}
/// Sends the given `Arc`-ed handle owner off to the linger pool.
///
/// If `T` implements `Into<OwnedHandle>`, use `linger` instead.
pub fn linger_arc<T: AsHandle + Send + Sync>(arc: LingerableArc<T>) {
    linger_ent(QueueEnt::IndirectHandle(DynHandleOwner::from_arc(arc)))
}
fn linger_ent(h: QueueEnt) {
    if !HAS_PERSISTENT_THREAD.fetch_or(true, AcqRel) {
        spawn_persistent_thread(h);
    } else if let Err(h) = QUEUE.enqueue(h) {
        spawn_high_wm_thread(h);
    }
}

#[derive(Debug)]
#[repr(C)]
struct HandleOwnerPointee<T> {
    dtor: DropFn,
    value: T,
}
type DropFn = unsafe fn(*mut ());

/// An `Arc` with a heap layout prepared for [`linger_arc`] lingering.
#[derive(Debug)]
pub struct LingerableArc<T>(Arc<HandleOwnerPointee<T>>);
impl<T: AsHandle + Send + Sync> LingerableArc<T> {
    /// Wraps the given object in a [`LingerableArc`].
    pub fn new(value: T) -> Self {
        Self(Arc::new(HandleOwnerPointee {
            dtor: |slf: *mut ()| {
                // SAFETY: slf is the same pointer as the one returned by into_raw
                let slf = unsafe { Arc::from_raw(slf.cast::<Self>()) };
                let _ = c_wrappers::flush(slf.0.value.as_handle());
            },
            value,
        }))
    }
}
impl<T> Deref for LingerableArc<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T { &self.0.value }
}
impl<T> Clone for LingerableArc<T> {
    #[inline]
    fn clone(&self) -> Self { Self(self.0.clone()) }
}

/// `Box<dyn AsHandle + Send + Sync>` (or the corresponding `Arc`) that fits
/// into a single pointer. In other words, `thin_trait_object` at home.
struct DynHandleOwner(*mut ());
// SAFETY: bound on HandleOwner ctor
unsafe impl Send for DynHandleOwner {}
unsafe impl Sync for DynHandleOwner {}
impl DynHandleOwner {
    fn boxed<T: AsHandle + Send + Sync>(value: T) -> Self {
        // FUTURE use const {}
        assert!(align_of::<Self>() >= 2, "cannot perform low-bit tagging in QueueEnt");
        let boxptr = Box::into_raw(Box::new(HandleOwnerPointee {
            dtor: |slf: *mut ()| {
                // SAFETY: slf is the same pointer as the one returned by into_raw
                let slf = unsafe { Box::from_raw(slf.cast::<HandleOwnerPointee<T>>()) };
                let _ = c_wrappers::flush(slf.value.as_handle());
            },
            value,
        }));
        Self(boxptr.cast())
    }
    fn from_arc<T: AsHandle + Send + Sync>(arc: LingerableArc<T>) -> Self {
        Self(Arc::into_raw(arc.0).cast_mut().cast())
    }
    fn into_raw(self) -> *mut () { ManuallyDrop::new(self).0 }
    unsafe fn from_raw(ptr: *mut ()) -> Self { Self(ptr) }
}
impl Drop for DynHandleOwner {
    fn drop(&mut self) {
        // SAFETY: DropFn is the first field of HandleOwnerPointee
        let dtor = unsafe { *self.0.cast_const().cast::<DropFn>() };
        unsafe { (dtor)(self.0) }
    }
}

struct HandleFini(OwnedHandle);
impl Drop for HandleFini {
    fn drop(&mut self) { let _ = c_wrappers::flush(self.0.as_handle()); }
}

enum QueueEnt {
    Handle(HandleFini),
    IndirectHandle(DynHandleOwner),
}
impl QueueEnt {
    /// Converts into a low-bit-tagged pointer.
    #[allow(clippy::as_conversions)] // FUTURE use Strict Provenance API
    fn into_raw(self) -> *mut () {
        match self {
            // Windows handles don't conflict with low-bit-tagging because
            // the OS guarantees that they're all multiples of 4
            Self::Handle(h) => ManuallyDrop::new(h).0.as_raw_handle().cast(),
            // This variant doesn't conflict with low-bit-tagging because we've
            // asserted that the alignment of boxed handle owners is at least 2
            // in the constructor of BoxedHandleOwner
            Self::IndirectHandle(bh) => (bh.into_raw() as usize | 1) as *mut (),
        }
    }
    /// Converts from a low-bit-tagged pointer created by `into_raw`.
    #[allow(clippy::as_conversions)] // FUTURE use Strict Provenance API
    unsafe fn from_raw(raw: *mut ()) -> Self {
        if raw as usize & 1 == 1 {
            let raw = (raw as usize & !1) as *mut ();
            Self::IndirectHandle(unsafe { DynHandleOwner::from_raw(raw) })
        } else {
            Self::Handle(HandleFini(unsafe { OwnedHandle::from_raw_handle(raw.cast()) }))
        }
    }
}

struct Queue {
    mtx: Mutex<QueueInner>,
    cv: Condvar,
}
impl Queue {
    const fn new() -> Self { Self { mtx: Mutex::new(QueueInner::new()), cv: Condvar::new() } }
    fn enqueue(&self, h: QueueEnt) -> Result<(), QueueEnt> {
        self.mtx.lock().unwrap_or_else(PoisonError::into_inner).enqueue(h)?;
        self.cv.notify_one();
        Ok(())
    }
    fn get(&self) -> (QueueEnt, bool) { self.lk_loop(QueueInner::dequeue_and_check_watermark) }
    fn get_timeout(&self, timeout: Duration) -> (Option<(QueueEnt, bool)>, Duration) {
        self.lk_loop_timeout(QueueInner::dequeue_and_check_watermark, timeout)
    }
    fn lk_loop<T>(&self, mut f: impl FnMut(&mut QueueInner) -> Option<T>) -> T {
        let mut lkr = self.mtx.lock();
        loop {
            let mut lk = lkr.unwrap_or_else(PoisonError::into_inner);
            if let Some(h) = f(&mut lk) {
                return h;
            }
            lkr = self.cv.wait(lk);
        }
    }
    fn lk_loop_timeout<T>(
        &self,
        mut f: impl FnMut(&mut QueueInner) -> Option<T>,
        mut timeout: Duration,
    ) -> (Option<T>, Duration) {
        let mut total_elapsed = Duration::ZERO;
        let mut lk = self.mtx.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ret) = f(&mut lk) {
            return (Some(ret), total_elapsed);
        }
        let first_ts = Instant::now();
        let mut before_wait = first_ts;

        let rslt = loop {
            let timor;
            (lk, timor) =
                self.cv.wait_timeout(lk, timeout).unwrap_or_else(PoisonError::into_inner);
            let after_wait = Instant::now();
            total_elapsed = after_wait.duration_since(first_ts);
            timeout = timeout.saturating_sub(after_wait.duration_since(before_wait));
            let false = (timor.timed_out() || timeout.is_zero()) else { break None };
            before_wait = after_wait;

            if let Some(ret) = f(&mut lk) {
                break Some(ret);
            }
        };
        (rslt, total_elapsed)
    }
}

struct QueueInner {
    queue: VecDeque<*mut ()>,
}
// SAFETY: the pointer bijects to QueueEnt, see where clauses below
unsafe impl Send for QueueInner where QueueEnt: Send {}
unsafe impl Sync for QueueInner where QueueEnt: Sync {}
impl QueueInner {
    const HIGH_WATERMARK: usize = 64;
    const LOW_WATERMARK: usize = 8;

    const fn new() -> Self { Self { queue: VecDeque::new() } }
    fn enqueue(&mut self, e: QueueEnt) -> Result<(), QueueEnt> {
        if self.queue.len() >= Self::HIGH_WATERMARK {
            return Err(e);
        }
        self.queue.reserve_exact(Self::HIGH_WATERMARK);
        self.queue.push_back(e.into_raw());
        Ok(())
    }
    fn dequeue(&mut self) -> Option<QueueEnt> {
        self.queue.pop_front().map(|p| unsafe { QueueEnt::from_raw(p) })
    }
    fn dequeue_and_check_watermark(&mut self) -> Option<(QueueEnt, bool)> {
        self.dequeue().map(|ent| (ent, self.above_low_watermark()))
    }
    fn above_low_watermark(&self) -> bool { self.queue.len() > Self::LOW_WATERMARK }
}

fn spawn_persistent_thread(h: QueueEnt) {
    spawn("linger pool (persist.)", persistent_thread_main, Some(h))
        .expect("failed to start the persistent thread of the Interprocess linger pool");
}
fn spawn_low_wm_thread() { let _ = spawn("linger pool", temporary_thread_main, None); }
fn spawn_high_wm_thread(h: QueueEnt) {
    let _ = spawn("linger pool", temporary_thread_main, Some(h));
}

const TEMP_TIMEOUT: Duration = Duration::from_millis(500);

fn persistent_thread_main(first_h: Option<QueueEnt>) {
    drop(first_h);
    loop {
        let (h, above_wm) = QUEUE.get();
        drop(h);
        if above_wm {
            spawn_low_wm_thread();
        }
    }
}

fn temporary_thread_main(first_h: Option<QueueEnt>) {
    drop(first_h);
    loop {
        let Some((h, above_wm)) = QUEUE.get_timeout(TEMP_TIMEOUT).0 else { return };
        drop(h);
        if above_wm {
            spawn_low_wm_thread();
        }
    }
}

#[inline(never)]
#[cold]
fn spawn(nm: &str, main: fn(Option<QueueEnt>), first: Option<QueueEnt>) -> io::Result<Thread> {
    std::thread::Builder::new()
        // FUTURE .no_hooks()
        .stack_size(128 * 1024)
        .name(nm.to_owned())
        .spawn(move || main(first))
        .map(|jh| jh.thread().clone())
}
