#[allow(unused_imports)]
use core::iter::{DoubleEndedIterator, ExactSizeIterator, FusedIterator};

/// A draining iterator for string data with unknown encoding.
#[derive(Debug)]
pub struct Drain<'a, T> {
    pub(crate) inner: alloc::vec::Drain<'a, T>,
}

impl<T> AsRef<[T]> for Drain<'_, T> {
    #[inline]
    fn as_ref(&self) -> &[T] {
        self.inner.as_ref()
    }
}

impl<T> Iterator for Drain<'_, T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<T> DoubleEndedIterator for Drain<'_, T> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.inner.next_back()
    }
}

impl<T> ExactSizeIterator for Drain<'_, T> {
    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl<T> FusedIterator for Drain<'_, T> {}
