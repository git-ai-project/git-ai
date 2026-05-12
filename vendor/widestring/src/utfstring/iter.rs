use super::{Utf16String, Utf32String};
use crate::utfstr::{CharsUtf16, CharsUtf32};
#[allow(unused_imports)]
use core::iter::{DoubleEndedIterator, ExactSizeIterator, FusedIterator, Iterator};

/// A draining iterator for [`Utf16String`].
///
/// This struct is created by the [`drain`][Utf16String::drain] method on [`Utf16String`]. See its
/// documentation for more.
pub struct DrainUtf16<'a> {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) iter: CharsUtf16<'a>,
    pub(super) string: *mut Utf16String,
}

unsafe impl Sync for DrainUtf16<'_> {}
unsafe impl Send for DrainUtf16<'_> {}

impl Drop for DrainUtf16<'_> {
    fn drop(&mut self) {
        unsafe {
            // Use Vec::drain. "Reaffirm" the bounds checks to avoid
            // panic code being inserted again.
            let self_vec = (*self.string).as_mut_vec();
            if self.start <= self.end && self.end <= self_vec.len() {
                self_vec.drain(self.start..self.end);
            }
        }
    }
}

impl core::fmt::Debug for DrainUtf16<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.iter, f)
    }
}

impl core::fmt::Display for DrainUtf16<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.iter, f)
    }
}

impl Iterator for DrainUtf16<'_> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl DoubleEndedIterator for DrainUtf16<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back()
    }
}

impl FusedIterator for DrainUtf16<'_> {}

/// A draining iterator for [`Utf32String`].
///
/// This struct is created by the [`drain`][Utf32String::drain] method on [`Utf32String`]. See its
/// documentation for more.
pub struct DrainUtf32<'a> {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) iter: CharsUtf32<'a>,
    pub(super) string: *mut Utf32String,
}

unsafe impl Sync for DrainUtf32<'_> {}
unsafe impl Send for DrainUtf32<'_> {}

impl Drop for DrainUtf32<'_> {
    fn drop(&mut self) {
        unsafe {
            // Use Vec::drain. "Reaffirm" the bounds checks to avoid
            // panic code being inserted again.
            let self_vec = (*self.string).as_mut_vec();
            if self.start <= self.end && self.end <= self_vec.len() {
                self_vec.drain(self.start..self.end);
            }
        }
    }
}

impl core::fmt::Debug for DrainUtf32<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.iter, f)
    }
}

impl core::fmt::Display for DrainUtf32<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.iter, f)
    }
}

impl Iterator for DrainUtf32<'_> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl DoubleEndedIterator for DrainUtf32<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back()
    }
}

impl FusedIterator for DrainUtf32<'_> {}

impl ExactSizeIterator for DrainUtf32<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}
