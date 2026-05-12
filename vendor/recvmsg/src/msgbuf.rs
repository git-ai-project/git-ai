//! Message reception buffers that can either contain a borrowed slice or own a memory allocation,
//! with customizable growth behavior, including memory quotas.
//!
//! See [`MsgBuf`]'s documentation.
#![allow(unsafe_code)]

// TODO fallible allocation

mod cap;
mod ctor;
mod cursors;
mod impl_debug;
mod lifetime;
mod owned;
mod quota_err;
mod safe_write;
mod slicing;
mod take;
#[cfg(test)]
mod tests;

pub use {owned::*, quota_err::*};

use core::{marker::PhantomData, mem::MaybeUninit, panic::UnwindSafe, ptr::NonNull};

type MuU8 = MaybeUninit<u8>;

/// A message reception buffer that can either contain a borrowed slice or own a memory allocation,
/// with customizable growth behavior, including memory quotas.
///
/// This type can be created from a buffer that lives on the stack:
/// ```
/// # use {core::mem::MaybeUninit, recvmsg::MsgBuf};
/// // An uninitialized buffer:
/// let mut arr = [MaybeUninit::new(0); 32];
/// let buf = MsgBuf::from(arr.as_mut());
/// assert_eq!(buf.capacity(), 32);
/// assert_eq!(buf.init_part().len(), 0); // Assumes nothing about the buffer.
/// assert_eq!(buf.filled_part().len(), 0);
/// assert!(!buf.has_msg); // No message is contained.
///
/// // A fully initialized buffer:
/// let mut arr = [0; 32];
/// let buf = MsgBuf::from(arr.as_mut());
/// assert_eq!(buf.capacity(), 32);
/// // Whole buffer can be passed to methods that take &mut [u8]:
/// assert_eq!(buf.init_part().len(), 32);
/// // Whole buffer is not assumed to be filled:
/// assert_eq!(buf.filled_part().len(), 0);
/// assert!(!buf.has_msg); // Not assumed to already contain a single received message.
/// ```
///
/// Or one on the heap via [`Box`]:
/// ```
/// # extern crate alloc;
/// # use {alloc::boxed::Box, core::mem::MaybeUninit, recvmsg::MsgBuf};
/// // An uninitialized buffer (yes, the annotations are necessary):
/// let buf = MsgBuf::from(Box::<[MaybeUninit<_>]>::from([MaybeUninit::new(0); 32]));
/// assert_eq!(buf.capacity(), 32);
/// assert_eq!(buf.len_init(), 0);
/// assert_eq!(buf.len_filled(), 0);
/// assert!(!buf.has_msg);
///
/// // A fully initialized buffer:
/// let buf = MsgBuf::from(Box::<[u8]>::from([0; 32]));
/// assert_eq!(buf.capacity(), 32);
/// assert_eq!(buf.len_init(), 32);
/// assert_eq!(buf.len_filled(), 0);
/// assert!(!buf.has_msg);
/// ```
///
/// Or in a `Vec`:
/// ```
/// # extern crate alloc;
/// # use {alloc::vec::Vec, core::mem::MaybeUninit, recvmsg::MsgBuf};
/// // An uninitialized buffer:
/// let buf = MsgBuf::from(Vec::with_capacity(31)); // Size can be odd too!
/// assert_eq!(buf.capacity(), 31);
/// assert_eq!(buf.len_init(), 0);
/// assert_eq!(buf.len_filled(), 0);
/// assert!(!buf.has_msg);
///
/// // A partially initialized buffer:
/// let mut vec = Vec::with_capacity(32);
/// vec.resize(6, 0);
/// let buf = MsgBuf::from(vec);
/// assert_eq!(buf.capacity(), 32);
/// assert_eq!(buf.len_init(), 6);
/// assert_eq!(buf.len_filled(), 0);
/// assert!(!buf.has_msg);
/// ```
pub struct MsgBuf<'slice> {
    ptr: NonNull<u8>,
    // All cursors count from `ptr`, not from each other.
    /// How much is allocated.
    cap: usize,
    /// How much is initialized. May not exceed `cap`.
    init: usize,
    /// Designates whether the buffer is borrowed or owned. `Option` is completely decorative and
    /// acts as a fancy boolean here.
    borrow: Option<PhantomData<&'slice mut [MuU8]>>,
    own_vt: &'static OwnedBufVtable,
    /// The length of the logically filled part of the buffer. Usually equal to the length of the
    /// last received message. May not exceed `init`.
    fill: usize,
    /// Whether the buffer has already been used to receive a message. Set to `false` to
    /// semantically invalidate the stored data.
    pub has_msg: bool,
    /// Highest allowed capacity for growth operations. This will only take effect on the next
    /// memory allocation.
    ///
    /// Note that `Vec` may slightly overshoot this quota due to amortization heuristics or simply
    /// due to the allocator providing excess capacity. This is accounted for via use of
    /// [`Vec::reserve_exact()`] instead of [`Vec::reserve()`] when the requested capacity is within
    /// a factor of two from the quota, which is modelled after `Vec`'s actual exponential growth
    /// behavior and thus should prevent overshoots in all but the most exceptional of situations.
    ///
    /// A `Some(0)` quota prevents allocation altogether.
    pub quota: Option<usize>,
}
// Who else remembers that this trait is a thing?
impl UnwindSafe for MsgBuf<'_> {}

unsafe impl Send for MsgBuf<'_> {}
unsafe impl Sync for MsgBuf<'_> {}

impl Drop for MsgBuf<'_> {
    fn drop(&mut self) {
        self.take_owned(); // If owned, returns `Some(vec)`, which is then dropped.
    }
}

/// Base pointer getters.
impl MsgBuf<'_> {
    /// Returns the base of the buffer as a const-pointer.
    #[inline(always)]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }
    /// Returns the base of the buffer as a pointer.
    #[inline(always)]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}
