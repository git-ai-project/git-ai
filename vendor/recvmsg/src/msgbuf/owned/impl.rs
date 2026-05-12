use super::{OwnedBuf, OwnedBufRawParts};
use core::ptr::NonNull;

unsafe impl OwnedBuf for alloc::vec::Vec<u8> {
    #[inline]
    unsafe fn from_raw_parts(OwnedBufRawParts { ptr, cap, init }: OwnedBufRawParts) -> Self {
        unsafe { Self::from_raw_parts(ptr.as_ptr(), init, cap) }
    }
    #[inline]
    fn as_raw_parts(&self) -> OwnedBufRawParts {
        OwnedBufRawParts {
            ptr: unsafe {
                // SAFETY: Vec base is never null
                NonNull::new_unchecked(self.as_ptr().cast_mut())
            },
            cap: self.capacity(),
            init: self.len(),
        }
    }
    fn grow(&mut self, new_cap: usize) {
        let incr = new_cap.saturating_sub(self.len());
        self.reserve_exact(incr)
    }
}

unsafe impl OwnedBuf for () {
    #[inline]
    unsafe fn from_raw_parts(_: OwnedBufRawParts) -> Self {}
    #[inline]
    fn as_raw_parts(&self) -> OwnedBufRawParts {
        OwnedBufRawParts { ptr: NonNull::dangling(), cap: 0, init: 0 }
    }
    fn grow(&mut self, _: usize) {
        panic!("buffer not allowed to allocate")
    }
}
