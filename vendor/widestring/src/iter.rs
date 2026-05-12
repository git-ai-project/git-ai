//! Iterators for encoding and decoding slices of string data.

use crate::{
    decode_utf16_surrogate_pair,
    error::{DecodeUtf16Error, DecodeUtf32Error},
    is_utf16_high_surrogate, is_utf16_low_surrogate, is_utf16_surrogate,
};
#[allow(unused_imports)]
use core::{
    char,
    iter::{DoubleEndedIterator, ExactSizeIterator, FusedIterator},
};

/// An iterator that decodes UTF-16 encoded code points from an iterator of [`u16`]s.
///
/// This struct is created by [`decode_utf16`][crate::decode_utf16]. See its documentation for more.
///
/// This struct is identical to [`char::DecodeUtf16`] except it is a [`DoubleEndedIterator`] if
/// `I` is.
#[derive(Debug, Clone)]
pub struct DecodeUtf16<I>
where
    I: Iterator<Item = u16>,
{
    iter: I,
    forward_buf: Option<u16>,
    back_buf: Option<u16>,
}

impl<I> DecodeUtf16<I>
where
    I: Iterator<Item = u16>,
{
    pub(crate) fn new(iter: I) -> Self {
        Self {
            iter,
            forward_buf: None,
            back_buf: None,
        }
    }
}

impl<I> Iterator for DecodeUtf16<I>
where
    I: Iterator<Item = u16>,
{
    type Item = Result<char, DecodeUtf16Error>;

    fn next(&mut self) -> Option<Self::Item> {
        // Copied from char::DecodeUtf16
        let u = match self.forward_buf.take() {
            Some(buf) => buf,
            None => self.iter.next().or_else(|| self.back_buf.take())?,
        };

        if !is_utf16_surrogate(u) {
            // SAFETY: not a surrogate
            Some(Ok(unsafe { char::from_u32_unchecked(u as u32) }))
        } else if is_utf16_low_surrogate(u) {
            // a trailing surrogate
            Some(Err(DecodeUtf16Error::new(u)))
        } else {
            let u2 = match self.iter.next().or_else(|| self.back_buf.take()) {
                Some(u2) => u2,
                // eof
                None => return Some(Err(DecodeUtf16Error::new(u))),
            };
            if !is_utf16_low_surrogate(u2) {
                // not a trailing surrogate so we're not a valid
                // surrogate pair, so rewind to redecode u2 next time.
                self.forward_buf = Some(u2);
                return Some(Err(DecodeUtf16Error::new(u)));
            }

            // all ok, so lets decode it.
            // SAFETY: verified the surrogate pair
            unsafe { Some(Ok(decode_utf16_surrogate_pair(u, u2))) }
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (low, high) = self.iter.size_hint();
        // we could be entirely valid surrogates (2 elements per
        // char), or entirely non-surrogates (1 element per char)
        (low / 2, high)
    }
}

impl<I> DoubleEndedIterator for DecodeUtf16<I>
where
    I: Iterator<Item = u16> + DoubleEndedIterator,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        let u2 = match self.back_buf.take() {
            Some(buf) => buf,
            None => self.iter.next_back().or_else(|| self.forward_buf.take())?,
        };

        if !is_utf16_surrogate(u2) {
            // SAFETY: not a surrogate
            Some(Ok(unsafe { char::from_u32_unchecked(u2 as u32) }))
        } else if is_utf16_high_surrogate(u2) {
            // a leading surrogate
            Some(Err(DecodeUtf16Error::new(u2)))
        } else {
            let u = match self.iter.next_back().or_else(|| self.forward_buf.take()) {
                Some(u) => u,
                // eof
                None => return Some(Err(DecodeUtf16Error::new(u2))),
            };
            if !is_utf16_high_surrogate(u) {
                // not a leading surrogate so we're not a valid
                // surrogate pair, so rewind to redecode u next time.
                self.back_buf = Some(u);
                return Some(Err(DecodeUtf16Error::new(u2)));
            }

            // all ok, so lets decode it.
            // SAFETY: verified the surrogate pair
            unsafe { Some(Ok(decode_utf16_surrogate_pair(u, u2))) }
        }
    }
}

impl<I> FusedIterator for DecodeUtf16<I> where I: Iterator<Item = u16> + FusedIterator {}

/// An iterator that lossily decodes possibly ill-formed UTF-16 encoded code points from an iterator
/// of [`u16`]s.
///
/// Any unpaired UTF-16 surrogate values are replaced by
/// [`U+FFFD REPLACEMENT_CHARACTER`][char::REPLACEMENT_CHARACTER] (�).
#[derive(Debug, Clone)]
pub struct DecodeUtf16Lossy<I>
where
    I: Iterator<Item = u16>,
{
    pub(crate) iter: DecodeUtf16<I>,
}

impl<I> Iterator for DecodeUtf16Lossy<I>
where
    I: Iterator<Item = u16>,
{
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|res| res.unwrap_or(char::REPLACEMENT_CHARACTER))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<I> DoubleEndedIterator for DecodeUtf16Lossy<I>
where
    I: Iterator<Item = u16> + DoubleEndedIterator,
{
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter
            .next_back()
            .map(|res| res.unwrap_or(char::REPLACEMENT_CHARACTER))
    }
}

impl<I> FusedIterator for DecodeUtf16Lossy<I> where I: Iterator<Item = u16> + FusedIterator {}

/// An iterator that decodes UTF-32 encoded code points from an iterator of `u32`s.
#[derive(Debug, Clone)]
pub struct DecodeUtf32<I>
where
    I: Iterator<Item = u32>,
{
    pub(crate) iter: I,
}

impl<I> Iterator for DecodeUtf32<I>
where
    I: Iterator<Item = u32>,
{
    type Item = Result<char, DecodeUtf32Error>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|u| char::from_u32(u).ok_or_else(|| DecodeUtf32Error::new(u)))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<I> DoubleEndedIterator for DecodeUtf32<I>
where
    I: Iterator<Item = u32> + DoubleEndedIterator,
{
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter
            .next_back()
            .map(|u| char::from_u32(u).ok_or_else(|| DecodeUtf32Error::new(u)))
    }
}

impl<I> FusedIterator for DecodeUtf32<I> where I: Iterator<Item = u32> + FusedIterator {}

impl<I> ExactSizeIterator for DecodeUtf32<I>
where
    I: Iterator<Item = u32> + ExactSizeIterator,
{
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}

/// An iterator that lossily decodes possibly ill-formed UTF-32 encoded code points from an iterator
/// of `u32`s.
///
/// Any invalid UTF-32 values are replaced by
/// [`U+FFFD REPLACEMENT_CHARACTER`][core::char::REPLACEMENT_CHARACTER] (�).
#[derive(Debug, Clone)]
pub struct DecodeUtf32Lossy<I>
where
    I: Iterator<Item = u32>,
{
    pub(crate) iter: DecodeUtf32<I>,
}

impl<I> Iterator for DecodeUtf32Lossy<I>
where
    I: Iterator<Item = u32>,
{
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter
            .next()
            .map(|res| res.unwrap_or(core::char::REPLACEMENT_CHARACTER))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<I> DoubleEndedIterator for DecodeUtf32Lossy<I>
where
    I: Iterator<Item = u32> + DoubleEndedIterator,
{
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter
            .next_back()
            .map(|res| res.unwrap_or(core::char::REPLACEMENT_CHARACTER))
    }
}

impl<I> FusedIterator for DecodeUtf32Lossy<I> where I: Iterator<Item = u32> + FusedIterator {}

impl<I> ExactSizeIterator for DecodeUtf32Lossy<I>
where
    I: Iterator<Item = u32> + ExactSizeIterator,
{
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}

/// An iterator that encodes an iterator of [`char`][prim@char]s into UTF-8 bytes.
///
/// This struct is created by [`encode_utf8`][crate::encode_utf8]. See its documentation for more.
#[derive(Debug, Clone)]
pub struct EncodeUtf8<I>
where
    I: Iterator<Item = char>,
{
    iter: I,
    buf: [u8; 4],
    idx: u8,
    len: u8,
}

impl<I> EncodeUtf8<I>
where
    I: Iterator<Item = char>,
{
    pub(crate) fn new(iter: I) -> Self {
        Self {
            iter,
            buf: [0; 4],
            idx: 0,
            len: 0,
        }
    }
}

impl<I> Iterator for EncodeUtf8<I>
where
    I: Iterator<Item = char>,
{
    type Item = u8;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.len {
            let c = self.iter.next()?;
            self.idx = 0;
            self.len = c.encode_utf8(&mut self.buf).len() as u8;
        }
        self.idx += 1;
        let idx = (self.idx - 1) as usize;
        Some(self.buf[idx])
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (lower, upper) = self.iter.size_hint();
        (lower, upper.and_then(|len| len.checked_mul(4))) // Max 4 UTF-8 bytes per char
    }
}

impl<I> FusedIterator for EncodeUtf8<I> where I: Iterator<Item = char> + FusedIterator {}

/// An iterator that encodes an iterator of [`char`][prim@char]s into UTF-16 [`u16`] code units.
///
/// This struct is created by [`encode_utf16`][crate::encode_utf16]. See its documentation for more.
#[derive(Debug, Clone)]
pub struct EncodeUtf16<I>
where
    I: Iterator<Item = char>,
{
    iter: I,
    buf: Option<u16>,
}

impl<I> EncodeUtf16<I>
where
    I: Iterator<Item = char>,
{
    pub(crate) fn new(iter: I) -> Self {
        Self { iter, buf: None }
    }
}

impl<I> Iterator for EncodeUtf16<I>
where
    I: Iterator<Item = char>,
{
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.buf.take().or_else(|| {
            let c = self.iter.next()?;
            let mut buf = [0; 2];
            let buf = c.encode_utf16(&mut buf);
            if buf.len() > 1 {
                self.buf = Some(buf[1]);
            }
            Some(buf[0])
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let (lower, upper) = self.iter.size_hint();
        (lower, upper.and_then(|len| len.checked_mul(2))) // Max 2 UTF-16 code units per char
    }
}

impl<I> FusedIterator for EncodeUtf16<I> where I: Iterator<Item = char> + FusedIterator {}

/// An iterator that encodes an iterator of [`char`][prim@char]s into UTF-32 [`u32`] values.
///
/// This struct is created by [`encode_utf32`][crate::encode_utf32]. See its documentation for more.
#[derive(Debug, Clone)]
pub struct EncodeUtf32<I>
where
    I: Iterator<Item = char>,
{
    iter: I,
}

impl<I> EncodeUtf32<I>
where
    I: Iterator<Item = char>,
{
    pub(crate) fn new(iter: I) -> Self {
        Self { iter }
    }
}

impl<I> Iterator for EncodeUtf32<I>
where
    I: Iterator<Item = char>,
{
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|c| c as u32)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<I> FusedIterator for EncodeUtf32<I> where I: Iterator<Item = char> + FusedIterator {}

impl<I> ExactSizeIterator for EncodeUtf32<I>
where
    I: Iterator<Item = char> + ExactSizeIterator,
{
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}

impl<I> DoubleEndedIterator for EncodeUtf32<I>
where
    I: Iterator<Item = char> + DoubleEndedIterator,
{
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back().map(|c| c as u32)
    }
}
