use super::{
    owned::OwnedBuf, owned_default, DynOwnedBuf, MsgBuf, MuU8, OwnedBufRawParts, OwnedBufVtable,
};
use alloc::{boxed::Box, vec::Vec};
use core::{
    mem::{ManuallyDrop, MaybeUninit},
    slice,
    {marker::PhantomData, ptr::NonNull},
};

impl Default for MsgBuf<'_> {
    #[inline]
    fn default() -> Self {
        Self {
            ptr: NonNull::dangling(),
            cap: 0,
            init: 0,
            borrow: None,
            own_vt: OwnedBufVtable::DEFAULT,
            fill: 0,
            has_msg: false,
            quota: None,
        }
    }
}

/// Constructors.
impl<'slice> MsgBuf<'slice> {
    /// Creates an `MsgBuf` from an owned buffer of the given type.
    ///
    /// This uses `new_owned_dyn()` under the hood, and does nothing special.
    #[inline]
    pub fn new_owned<Owned: OwnedBuf>(buf: Owned) -> Self {
        Self::new_owned_dyn(DynOwnedBuf::new(buf))
    }
    /// Creates an `MsgBuf` from an owned buffer trait object.
    ///
    /// The resulting buffer has the same capacity and initialization cursor, but no quota.
    #[inline]
    pub fn new_owned_dyn(buf: DynOwnedBuf) -> Self {
        let mut slf = Self::default();
        slf.put_owned(buf);
        slf
    }

    /// Creates an `MsgBuf` by allocating an owned buffer of the given type and size.
    pub fn with_capacity<Owned: OwnedBuf>(cap: usize) -> Self {
        let mut owned = owned_default::<Owned>();
        owned.grow(cap);
        Self::new_owned(owned)
    }

    /// Forgets old buffer in place, if there was one, and replaces it with the given `owned`.
    pub(super) fn put_owned(&mut self, owned: DynOwnedBuf) {
        (OwnedBufRawParts { ptr: self.ptr, cap: self.cap, init: self.init }, self.own_vt) =
            owned.into_raw_and_vt();
        self.borrow = None;
        self.fill = 0;
    }
    /// Forgets old buffer in place, if there was one, and replaces it with the given `slice`.
    fn put_slice(&mut self, slice: &'slice mut [MuU8]) {
        self.ptr = NonNull::new(slice.as_mut_ptr().cast()).unwrap_or(NonNull::dangling());
        self.cap = slice.len();
        self.borrow = Some(PhantomData);
        self.init = 0;
        self.fill = 0;
    }
}

/// Sets `init` = `owned.len()`.
impl<Owned: OwnedBuf> From<Owned> for MsgBuf<'_> {
    #[inline]
    fn from(owned: Owned) -> Self {
        Self::new_owned(owned)
    }
}

impl From<DynOwnedBuf> for MsgBuf<'_> {
    #[inline]
    fn from(owned: DynOwnedBuf) -> Self {
        Self::new_owned_dyn(owned)
    }
}

impl<'slice> From<&'slice mut [MaybeUninit<u8>]> for MsgBuf<'slice> {
    #[inline]
    fn from(borrowed: &'slice mut [MaybeUninit<u8>]) -> Self {
        let mut slf = Self::default();
        slf.put_slice(borrowed);
        slf
    }
}
impl From<Box<[MaybeUninit<u8>]>> for MsgBuf<'_> {
    fn from(bx: Box<[MaybeUninit<u8>]>) -> Self {
        let mut muvec = ManuallyDrop::new(Vec::from(bx));
        unsafe { Vec::from_raw_parts(muvec.as_mut_ptr().cast::<u8>(), 0, muvec.capacity()) }.into()
    }
}

/// Sets `init` = `borrowed.len()`.
impl<'slice> From<&'slice mut [u8]> for MsgBuf<'slice> {
    #[inline]
    fn from(borrowed: &'slice mut [u8]) -> Self {
        let (base, len) = (borrowed.as_mut_ptr(), borrowed.len());
        let mut slf: Self = unsafe { slice::from_raw_parts_mut(base.cast::<MuU8>(), len) }.into();
        unsafe { slf.set_init(slf.cap) };
        slf
    }
}
/// Sets `init` = `bx.len()`.
impl From<Box<[u8]>> for MsgBuf<'_> {
    #[inline]
    fn from(bx: Box<[u8]>) -> Self {
        Vec::from(bx).into()
    }
}
