#![allow(dead_code)]

#[cfg(unix)]
use std::os::unix::io::RawFd;
use std::{
    io,
    mem::{size_of, ManuallyDrop, MaybeUninit},
    num::{NonZeroU8, Saturating},
    ops::ControlFlow,
    pin::Pin,
    slice,
    sync::PoisonError,
    task::{RawWaker, RawWakerVTable, Waker},
    time::{Duration, Instant},
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};

/// Utility trait that, if used as a supertrait, prevents other crates from implementing the
/// trait.
pub(crate) trait Sealed {}
pub(crate) trait DebugExpectExt: Sized {
    fn debug_expect(self, msg: &str);
}

pub(crate) static LOCK_POISON: &str = "unexpected lock poison";
pub(crate) fn poison_error<T>(_: PoisonError<T>) -> io::Error { io::Error::other(LOCK_POISON) }

pub(crate) trait OrErrno<T>: Sized {
    fn true_or_errno(self, f: impl FnOnce() -> T) -> io::Result<T>;
    #[inline(always)]
    fn true_val_or_errno(self, value: T) -> io::Result<T> { self.true_or_errno(|| value) }
    fn false_or_errno(self, f: impl FnOnce() -> T) -> io::Result<T>;
    #[inline(always)]
    fn false_val_or_errno(self, value: T) -> io::Result<T> { self.true_or_errno(|| value) }
}
impl<B: ToBool, T> OrErrno<T> for B {
    #[inline]
    fn true_or_errno(self, f: impl FnOnce() -> T) -> io::Result<T> {
        if self.to_bool() {
            Ok(f())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn false_or_errno(self, f: impl FnOnce() -> T) -> io::Result<T> {
        if !self.to_bool() {
            Ok(f())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(unix)]
pub(crate) trait FdOrErrno: Sized {
    fn fd_or_errno(self) -> io::Result<Self>;
}
#[cfg(unix)]
impl FdOrErrno for RawFd {
    #[inline]
    fn fd_or_errno(self) -> io::Result<Self> { (self != -1).true_val_or_errno(self) }
}

#[cfg(windows)]
pub(crate) trait HandleOrErrno: Sized {
    fn handle_or_errno(self) -> io::Result<Self>;
}
#[cfg(windows)]
impl HandleOrErrno for HANDLE {
    #[inline]
    fn handle_or_errno(self) -> io::Result<Self> {
        (self != INVALID_HANDLE_VALUE).true_val_or_errno(self)
    }
}

// FUTURE remove
pub(crate) trait ControlFlowExt {
    type B;
    type C;
    // "pf" means "polyfill"
    fn break_value_pf(self) -> Option<Self::B>;
    fn continue_value_pf(self) -> Option<Self::C>;
    fn is_break_pf(&self) -> bool;
    fn is_continue_pf(&self) -> bool;
}
impl<B, C> ControlFlowExt for ControlFlow<B, C> {
    type B = B;
    type C = C;
    #[inline(always)]
    fn break_value_pf(self) -> Option<B> {
        match self {
            Self::Break(v) => Some(v),
            Self::Continue(_) => None,
        }
    }
    #[inline(always)]
    fn continue_value_pf(self) -> Option<C> {
        match self {
            Self::Break(_) => None,
            Self::Continue(v) => Some(v),
        }
    }
    #[inline(always)]
    fn is_break_pf(&self) -> bool { matches!(self, Self::Break(..)) }
    #[inline(always)]
    fn is_continue_pf(&self) -> bool { matches!(self, Self::Continue(..)) }
}

pub(crate) trait OptionExt {
    type Value;
    fn break_some(self) -> ControlFlow<Self::Value>;
}
impl<T> OptionExt for Option<T> {
    type Value = T;
    fn break_some(self) -> ControlFlow<T> {
        match self {
            Some(v) => ControlFlow::Break(v),
            None => ControlFlow::Continue(()),
        }
    }
}

pub(crate) trait OptionTimeoutExt {
    type Output;
    fn some_or_timeout(self) -> io::Result<Self::Output>;
}
impl<O> OptionTimeoutExt for Option<io::Result<O>> {
    type Output = O;
    #[inline(always)]
    fn some_or_timeout(self) -> io::Result<O> {
        match self {
            Some(r) => r,
            None => Err(io::Error::from(io::ErrorKind::TimedOut)),
        }
    }
}

pub(crate) trait ToBool {
    fn to_bool(self) -> bool;
}
impl ToBool for bool {
    #[inline(always)]
    fn to_bool(self) -> bool { self }
}
impl ToBool for i32 {
    #[inline(always)]
    fn to_bool(self) -> bool { self != 0 }
}

pub(crate) trait BoolExt {
    fn to_i32(self) -> i32;
    fn to_usize(self) -> usize;
}
impl BoolExt for bool {
    #[inline(always)] #[rustfmt::skip] // oh come on now
    fn to_i32(self) -> i32 {
        if self { 1 } else { 0 }
    }
    #[inline(always)] #[rustfmt::skip]
    fn to_usize(self) -> usize {
        if self { 1 } else { 0 }
    }
}

// FUTURE replace with std::ptr::from_{ref,mut}
#[inline(always)]
pub(crate) const fn ref2ptr<T: ?Sized>(r: &T) -> *const T { r }
#[inline(always)]
pub(crate) fn mut2ptr<T: ?Sized>(r: &mut T) -> *mut T { r }

impl<T, E: std::fmt::Debug> DebugExpectExt for Result<T, E> {
    #[inline]
    #[track_caller]
    fn debug_expect(self, msg: &str) {
        if cfg!(debug_assertions) {
            self.expect(msg);
        }
    }
}
impl<T> DebugExpectExt for Option<T> {
    #[inline]
    #[track_caller]
    fn debug_expect(self, msg: &str) {
        if cfg!(debug_assertions) {
            self.expect(msg);
        }
    }
}

pub(crate) trait NumExt: Sized {
    #[inline]
    fn saturate(self) -> Saturating<Self> { Saturating(self) }
}
impl<T> NumExt for T {}

pub(crate) trait SubUsizeExt: TryInto<usize> + Sized {
    fn to_usize(self) -> usize;
}
pub(crate) trait SubIsizeExt: TryInto<usize> + Sized {
    fn to_isize(self) -> isize;
}
macro_rules! impl_subsize {
    ($src:ident to usize) => {
        impl SubUsizeExt for $src {
            #[inline(always)]
            fn to_usize(self) -> usize {
                self as usize
            }
        }
    };
    ($src:ident to isize) => {
        impl SubIsizeExt for $src {
            #[inline(always)]
            // we don't run on 16-bit platforms
            #[allow(clippy::cast_possible_wrap)]
            fn to_isize(self) -> isize {
                self as isize
            }
        }
    };
    ($($src:ident to $dst:ident)+) => {$(
        impl_subsize!($src to $dst);
    )+};
}
// See platform_check.rs.
impl_subsize! {
    u8  to usize
    u16 to usize
    u32 to usize
    i8  to isize
    i16 to isize
    i32 to isize
    u8  to isize
    u16 to isize
}

// TODO find a more elegant way
pub(crate) trait RawOsErrorExt {
    fn eeq(self, other: u32) -> bool;
}
impl RawOsErrorExt for Option<i32> {
    #[inline(always)]
    #[allow(clippy::cast_sign_loss)] // bitwise comparison
    fn eeq(self, other: u32) -> bool {
        match self {
            Some(n) => n as u32 == other,
            None => false,
        }
    }
}

/// Crudely casts a slice without any checks, blindly presuming that the size of `T` is equal to
/// that of `U`.
pub(crate) const unsafe fn cast_slice<T, U>(s: &[T]) -> &[U] {
    // FUTURE use const assertion
    if size_of::<T>() != size_of::<U>() {
        panic!("element sizes must be equal");
    }
    unsafe { slice::from_raw_parts(s.as_ptr().cast(), s.len()) }
}
/// Mutable version of [`cast_slice`].
pub(crate) unsafe fn cast_slice_mut<T, U>(s: &mut [T]) -> &mut [U] {
    // FUTURE use const assertion
    if size_of::<T>() != size_of::<U>() {
        panic!("element sizes must be equal");
    }
    unsafe { slice::from_raw_parts_mut(s.as_mut_ptr().cast(), s.len()) }
}

#[inline(always)]
// SAFETY: weaker refinement
pub(crate) fn weaken_buf_init<T>(s: &[T]) -> &[MaybeUninit<T>] { unsafe { cast_slice(s) } }

#[inline(always)]
pub(crate) unsafe fn assume_slice_init<T>(s: &[MaybeUninit<T>]) -> &[T] {
    // SAFETY: same slice, stronger refinement
    unsafe { cast_slice(s) }
}
#[inline(always)]
pub(crate) unsafe fn assume_slice_init_mut<T>(s: &mut [MaybeUninit<T>]) -> &mut [T] {
    // SAFETY: as above
    unsafe { cast_slice_mut(s) }
}

#[inline(always)]
pub(crate) fn contains_nuls(s: &[u8]) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::strnlen(s.as_ptr().cast(), s.len()) != s.len() }
    }
    #[cfg(not(unix))]
    {
        s.contains(&0)
    }
}
#[inline(always)]
pub(crate) const unsafe fn assume_nonzero_slice(s: &[u8]) -> &[NonZeroU8] {
    unsafe { cast_slice(s) }
}
#[inline(always)]
pub(crate) unsafe fn assume_nonzero_slice_mut(s: &mut [u8]) -> &mut [NonZeroU8] {
    unsafe { cast_slice_mut(s) }
}
#[inline(always)]
pub(crate) fn check_nonzero_slice(s: &[u8]) -> Option<&[NonZeroU8]> {
    let false = contains_nuls(s) else { return None };
    // SAFETY: we've just checked for nul bytes
    Some(unsafe { assume_nonzero_slice(s) })
}
#[inline(always)]
pub(crate) fn check_nonzero_slice_mut(s: &mut [u8]) -> Option<&mut [NonZeroU8]> {
    let false = contains_nuls(s) else { return None };
    // SAFETY: as above
    Some(unsafe { cast_slice_mut(s) })
}
// SAFETY: weaker refinement
#[inline(always)]
pub(crate) fn weaken_nonzero_slice(s: &[NonZeroU8]) -> &[u8] { unsafe { cast_slice(s) } }

pub(crate) trait UnpinExt: Unpin {
    #[inline]
    fn pin(&mut self) -> Pin<&mut Self> { Pin::new(self) }
}
impl<T: Unpin + ?Sized> UnpinExt for T {}

/// Generalizes over `&mut [u8]` and `&mut [MaybeUninit<u8>]`.
///
/// # Safety
/// The pointer returned by `as_ptr` must be valid for writes of length returned by a preceding
/// call to `len` for at least as long as no methods other than those that are in this trait are
/// called.
pub(crate) unsafe trait AsBuf {
    fn as_ptr(&mut self) -> *mut u8;
    fn len(&mut self) -> usize;
}
unsafe impl AsBuf for [u8] {
    #[inline(always)]
    fn as_ptr(&mut self) -> *mut u8 { self.as_mut_ptr() }
    #[inline(always)]
    fn len(&mut self) -> usize { <[u8]>::len(self) }
}
unsafe impl AsBuf for [MaybeUninit<u8>] {
    #[inline(always)]
    fn as_ptr(&mut self) -> *mut u8 { self.as_mut_ptr().cast() }
    #[inline(always)]
    fn len(&mut self) -> usize { <[MaybeUninit<u8>]>::len(self) }
}

pub(crate) fn spin_with_timeout<S, R>(
    state: &mut S,
    timeout: Option<Duration>,
    start: impl FnOnce(&mut S) -> ControlFlow<io::Result<R>>,
    spin: impl FnMut(&mut S, Option<Duration>) -> ControlFlow<io::Result<R>>,
    update_timeout: impl FnMut(&mut S, Duration),
) -> Option<io::Result<R>> {
    if let ControlFlow::Break(val) = start(state) {
        Some(val)
    } else {
        spin_with_timeout_loop(state, timeout, spin, update_timeout)
    }
}
#[cold]
fn spin_with_timeout_loop<S, R>(
    state: &mut S,
    mut timeout: Option<Duration>,
    mut spin: impl FnMut(&mut S, Option<Duration>) -> ControlFlow<io::Result<R>>,
    mut update_timeout: impl FnMut(&mut S, Duration),
) -> Option<io::Result<R>> {
    let end = match timeout.map(timeout_expiry).transpose() {
        Ok(end_or_none) => end_or_none,
        Err(e) => return Some(Err(e)),
    };

    loop {
        if let ControlFlow::Break(val) = spin(state, timeout) {
            break Some(val);
        }
        if let Some(end) = end {
            let cur = Instant::now();
            if cur >= end {
                update_timeout(state, Duration::ZERO);
                break None;
            }
            let remain = end.saturating_duration_since(cur);
            timeout = Some(remain);
            update_timeout(state, remain);
        }
    }
}

// FUTURE remove in favor of Waker::noop
#[inline(always)]
pub(crate) fn noop_waker() -> ManuallyDrop<Waker> {
    ManuallyDrop::new(unsafe { Waker::from_raw(noop_raw_waker()) })
}
#[inline(always)]
fn noop_raw_waker() -> RawWaker {
    static VTAB: RawWakerVTable = RawWakerVTable::new(|_| noop_raw_waker(), drop, drop, drop);
    RawWaker::new(std::ptr::null(), &VTAB)
}

pub(crate) fn timeout_expiry(timeout: Duration) -> io::Result<Instant> {
    let msg = "timeout expiry time overflowed std::time::Instant";
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, msg))
}

#[cfg(panic = "unwind")]
#[inline(never)]
#[cold]
#[track_caller]
pub(crate) fn aborting_panic<M: std::any::Any + Send + 'static>(m: M) -> ! {
    struct SilentBomb;
    impl Drop for SilentBomb {
        #[inline(always)]
        fn drop(&mut self) { std::process::abort() }
    }
    let _bomb = SilentBomb;
    // It would be pretty nice to be able to trigger the Usual Panic Things without proceeding
    // with an unwind, but this is the best that current Rust offers and it's not too terribly
    // expensive anyway.
    std::panic::panic_any(m)
}
#[cfg(not(panic = "unwind"))]
pub(crate) use std::panic::panic_any as aborting_panic;
