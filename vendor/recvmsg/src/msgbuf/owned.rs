mod grow_fn;
mod r#impl;
mod trait_object;
pub use {grow_fn::*, trait_object::*};

use core::{mem::ManuallyDrop, ptr::NonNull};

/// Owned buffers for use with [`MsgBuf`](super::MsgBuf).
///
/// Implementors can only be characterized by their three properties they're constructed from and
/// deconstructed into:
/// - Base pointer, the start of the buffer and the handle for reallocation and deallocation
/// - Capacity, the length of the buffer
/// - Initialization cursor, the number of bytes at the beginning of the buffer which can be assumed
///   to be well-initialized, and are to be retained when growing
///
/// # Contract
/// ## Logic
///  - For an `OwnedBuf` with init cursor 𝑖, after a call to `.grow()`, the first 𝑖 bytes starting
///   from the base pointer must match the corresponding values before the call. In other words,
///   growth must retain the contents of the initialized part.
/// - The initialization cursor position must not spuriously decrease.
/// - `.grow()` must increase the buffer's capacity to the given new capacity. It *may* decrease the
///   capacity if the new value is lower than the current capacity.
///
/// ## Safety
/// - For an `OwnedBuf` with capacity 𝑐, the first 𝑐 bytes starting from the base pointer must be
///   valid for reads of `MaybeUninit<u8>` given a shared reference to it and writes given an
///   exclusive reference.
/// - The buffer must not become invalidated unless its drop code is called. In particular, allowing
///   a buffer placed in [`ManuallyDrop`](core::mem::ManuallyDrop) to go out of scope may not
///   deallocate or otherwise invalidate the buffer.
///     - The base pointer may only change as a result of `.grow()`. It must not change on any other
///       occasion.
/// - Capacity must not spuriously decrease.
pub unsafe trait OwnedBuf: Send + Sync + Sized + 'static {
    /// Creates the owned buffer from its base pointer, capacity and the initialization cursor,
    /// consuming ownership. If `cap` is zero, the buffer is considered empty and the value of `ptr`
    /// is insignificant.
    ///
    /// # Safety
    /// - If `cap` is non-zero, `ptr` must be a value returned by an `into_raw_parts()` call on the
    ///   same type.
    /// - `ptr` must not be owned by any other instance of the type.
    /// - `init` must not exceed `cap`.
    unsafe fn from_raw_parts(raw_parts: OwnedBufRawParts) -> Self;
    /// Returns the raw parts the object is comprised of without relinquishing ownership.
    fn as_raw_parts(&self) -> OwnedBufRawParts;
    /// Grows the buffer up to the given capacity.
    ///
    /// Does not necessarily have to be able to decrease the buffer's capacity.
    fn grow(&mut self, new_cap: usize);
}

pub(crate) fn owned_into_raw_parts<T: OwnedBuf>(slf: T) -> OwnedBufRawParts {
    ManuallyDrop::new(slf).as_raw_parts()
}
pub(crate) fn owned_default<T: OwnedBuf>() -> T {
    unsafe { T::from_raw_parts(OwnedBufRawParts { ptr: NonNull::dangling(), cap: 0, init: 0 }) }
}

/// The raw parts of an [`OwnedBuf`].
#[derive(Copy, Clone, Debug)]
pub struct OwnedBufRawParts {
    /// Base pointer of the buffer.
    pub ptr: NonNull<u8>,
    /// The capacity of the buffer. This many bytes starting with `ptr` and going upward are owned
    /// by an `OwnedBuf` that returns this from `.as_raw_parts()`.
    pub cap: usize,
    /// The initialization cursor of the buffer. This many bytes starting with `ptr` and going
    /// upward are safe to read (known not to contain traces of previous memory allocations and
    /// internal allocator data structures). Must not exceed the capacity.
    pub init: usize,
}
unsafe impl Send for OwnedBufRawParts {}
unsafe impl Sync for OwnedBufRawParts {} // POD.
