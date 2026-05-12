use super::{MsgBuf, MuU8};
use core::{
    mem::transmute,
    ops::{Deref, DerefMut},
    slice,
};

unsafe fn assume_init_slice(slice: &[MuU8]) -> &[u8] {
    unsafe { transmute(slice) }
}
unsafe fn assume_init_slice_mut(slice: &mut [MuU8]) -> &mut [u8] {
    unsafe { transmute(slice) }
}

/// Borrows the whole buffer.
///
/// Not particularly useful, although the resulting slice can be sub-sliced and transmuted. Exists
/// primarily due to the bound on `DerefMut`.
impl Deref for MsgBuf<'_> {
    type Target = [MuU8];
    #[inline]
    fn deref(&self) -> &[MuU8] {
        unsafe { slice::from_raw_parts(self.as_ptr().cast(), self.cap) }
    }
}
/// Borrows the whole buffer.
impl DerefMut for MsgBuf<'_> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [MuU8] {
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr().cast(), self.cap) }
    }
}

/// Safe immutable access.
impl MsgBuf<'_> {
    /// Returns the most recently received message, or an empty slice if no message has been
    /// received yet.
    #[inline]
    pub fn msg(&self) -> Option<&[u8]> {
        self.has_msg.then(|| self.filled_part())
    }
}

/// Parts, slicing and splitting.
impl<'slice> MsgBuf<'slice> {
    /// Borrows the filled part of the buffer.
    #[inline]
    pub fn filled_part(&self) -> &[u8] {
        unsafe { assume_init_slice(&self[..self.fill]) }
    }
    /// Mutably borrows the filled part of the buffer.
    #[inline]
    pub fn filled_part_mut(&mut self) -> &mut [u8] {
        let fill = self.fill;
        unsafe { assume_init_slice_mut(&mut self[..fill]) }
    }

    /// Mutably borrows the part of the buffer which is initialized but unfilled.
    #[inline]
    pub fn init_but_unfilled_part_mut(&mut self) -> &mut [u8] {
        let (init, fill) = (self.init, self.fill);
        unsafe { assume_init_slice_mut(&mut self[fill..init]) }
    }

    /// Borrows the initialized (but potentially partially unfilled) part of the buffer.
    #[inline]
    pub fn init_part(&self) -> &[u8] {
        unsafe { assume_init_slice(&self[..self.init]) }
    }
    /// Mutably borrows the initialized (but potentially partially unfilled) part of the buffer.
    #[inline]
    pub fn init_part_mut(&mut self) -> &mut [u8] {
        let init = self.init;
        unsafe { assume_init_slice_mut(&mut self[..init]) }
    }

    /// Borrows the uninitialized part of the buffer.
    ///
    /// If you need this to be a buffer object, use `.split_at_init().1`.
    #[inline]
    pub fn uninit_part(&mut self) -> &mut [MuU8] {
        let init = self.init;
        &mut self[init..]
    }
    /// Borrows the unfilled part of the buffer.
    ///
    /// If you need this to be a buffer object, use `.split_at_fill().1`.
    #[inline]
    pub fn unfilled_part(&mut self) -> &mut [MuU8] {
        let fill = self.fill;
        &mut self[fill..]
    }
}
