// Déjà vu! I have been in this place before...

// FUTURE downcasting, requires TypeId in const contexts

use super::*;
use alloc::vec::Vec;

type VtGrow = unsafe fn(OwnedBufRawParts, usize) -> OwnedBufRawParts;
type VtDrop = unsafe fn(OwnedBufRawParts);

#[derive(Copy, Clone, Debug)]
pub(crate) struct OwnedBufVtable {
    grow: VtGrow,
    drop: VtDrop,
}
impl OwnedBufVtable {
    // TODO support no-alloc here
    pub(crate) const DEFAULT: &'static Self = Self::new::<Vec<u8>>();
    #[inline]
    const fn new<T: OwnedBuf>() -> &'static Self {
        unsafe fn vtgrow<T: OwnedBuf>(raw: OwnedBufRawParts, new_cap: usize) -> OwnedBufRawParts {
            let mut slf = unsafe { T::from_raw_parts(raw) };
            slf.grow(new_cap);
            owned_into_raw_parts(slf)
        }
        unsafe fn vtdrop<T: OwnedBuf>(raw: OwnedBufRawParts) {
            drop(unsafe { T::from_raw_parts(raw) });
        }

        &Self { grow: vtgrow::<T>, drop: vtdrop::<T> }
    }
}

/// Trait object for [`OwnedBuf`].
///
/// This allows [`MsgBuf`](crate::MsgBuf) to not be generic, allowing the message reception traits
/// to be object-safe.
#[derive(Debug)]
pub struct DynOwnedBuf {
    vt: &'static OwnedBufVtable,
    raw: OwnedBufRawParts,
}
unsafe impl Send for DynOwnedBuf {}
unsafe impl Sync for DynOwnedBuf {}
impl DynOwnedBuf {
    /// Erases the type of `buf` and turns it into a trait object.
    #[inline]
    pub fn new<T: OwnedBuf>(buf: T) -> Self {
        Self { vt: OwnedBufVtable::new::<T>(), raw: owned_into_raw_parts(buf) }
    }

    /// Returns the raw parts the object is comprised of without relinquishing ownership.
    #[inline]
    pub fn as_raw_parts(&self) -> OwnedBufRawParts {
        self.raw
    }

    /// Grows the buffer up to the given capacity.
    ///
    /// May or may not be able to decrease the buffer's capacity.
    ///
    /// # Panics
    /// If the owned buffer implementation successfully returns from `.grow()` but fails to produce
    /// a buffer of the requested capacity.
    #[inline]
    pub fn grow(&mut self, new_cap: usize) {
        // You may not like it, but this is what peak method call looks like
        let vt = self.vt;
        let raw = self.take();
        unsafe {
            self.raw = (self.vt.grow)(raw, new_cap);
            self.vt = vt;
        }
        let actual_cap = self.raw.cap;
        assert!(
            actual_cap >= new_cap,
            "growth function error (expected {new_cap} bytes or more, got {actual_cap})"
        );
    }

    /// Relinquishes ownership of the buffer and returns the raw parts, replacing `self` with
    /// an empty buffer of the same underlying type.
    #[inline]
    pub fn take(&mut self) -> OwnedBufRawParts {
        let raw = self.raw;
        (self.raw.cap, self.raw.init) = (0, 0);
        raw
    }

    pub(crate) fn into_raw_and_vt(self) -> (OwnedBufRawParts, &'static OwnedBufVtable) {
        let slf = ManuallyDrop::new(self);
        (slf.raw, slf.vt)
    }
    pub(crate) unsafe fn from_raw_and_vt(
        raw: OwnedBufRawParts,
        vt: &'static OwnedBufVtable,
    ) -> Self {
        Self { vt, raw }
    }
}
impl Drop for DynOwnedBuf {
    fn drop(&mut self) {
        unsafe { (self.vt.drop)(self.as_raw_parts()) }
    }
}
/// Creates an empty `Vec`-backed buffer.
impl Default for DynOwnedBuf {
    #[inline]
    fn default() -> Self {
        Self::new(Vec::new())
    }
}
