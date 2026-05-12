use crate::{
    debug_fmt_char_iter, decode_utf16, decode_utf32,
    iter::{DecodeUtf16, DecodeUtf32},
};
use core::{
    borrow::Borrow,
    iter::Peekable,
    ops::{Index, Range},
};
#[allow(unused_imports)]
use core::{
    fmt::Write,
    iter::{Copied, DoubleEndedIterator, ExactSizeIterator, FlatMap, FusedIterator},
    slice::Iter,
};

/// An iterator over the [`char`]s of a UTF-16 string slice
///
/// This struct is created by the [`chars`][crate::Utf16Str::chars] method on
/// [`Utf16Str`][crate::Utf16Str]. See its documentation for more.
#[derive(Clone)]
pub struct CharsUtf16<'a> {
    iter: DecodeUtf16<Copied<Iter<'a, u16>>>,
}

impl<'a> CharsUtf16<'a> {
    pub(super) fn new(s: &'a [u16]) -> Self {
        Self {
            iter: decode_utf16(s.iter().copied()),
        }
    }
}

impl Iterator for CharsUtf16<'_> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // Utf16Str already ensures valid surrogate pairs
        self.iter.next().map(|r| r.unwrap())
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl FusedIterator for CharsUtf16<'_> {}

impl DoubleEndedIterator for CharsUtf16<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back().map(|r| r.unwrap())
    }
}

impl core::fmt::Debug for CharsUtf16<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        debug_fmt_char_iter(self.clone(), f)
    }
}

impl core::fmt::Display for CharsUtf16<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.clone().try_for_each(|c| f.write_char(c))
    }
}

/// An iterator over the [`char`]s of a UTF-32 string slice
///
/// This struct is created by the [`chars`][crate::Utf32Str::chars] method on
/// [`Utf32Str`][crate::Utf32Str]. See its documentation for more.
#[derive(Clone)]
pub struct CharsUtf32<'a> {
    iter: DecodeUtf32<Copied<Iter<'a, u32>>>,
}

impl<'a> CharsUtf32<'a> {
    pub(super) fn new(s: &'a [u32]) -> Self {
        Self {
            iter: decode_utf32(s.iter().copied()),
        }
    }
}

impl Iterator for CharsUtf32<'_> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // Utf32Str already ensures valid code points
        self.iter.next().map(|r| r.unwrap())
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl DoubleEndedIterator for CharsUtf32<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        // Utf32Str already ensures valid code points
        self.iter.next_back().map(|r| r.unwrap())
    }
}

impl FusedIterator for CharsUtf32<'_> {}

impl ExactSizeIterator for CharsUtf32<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}

impl core::fmt::Debug for CharsUtf32<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        debug_fmt_char_iter(self.clone(), f)
    }
}

impl core::fmt::Display for CharsUtf32<'_> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.clone().try_for_each(|c| f.write_char(c))
    }
}

/// An iterator over the [`char`]s of a string slice, and their positions
///
/// This struct is created by the [`char_indices`][crate::Utf16Str::char_indices] method on
/// [`Utf16Str`][crate::Utf16Str]. See its documentation for more.
#[derive(Debug, Clone)]
pub struct CharIndicesUtf16<'a> {
    forward_offset: usize,
    back_offset: usize,
    iter: CharsUtf16<'a>,
}

impl CharIndicesUtf16<'_> {
    /// Returns the position of the next character, or the length of the underlying string if
    /// there are no more characters.
    #[inline]
    pub fn offset(&self) -> usize {
        self.forward_offset
    }
}

impl<'a> CharIndicesUtf16<'a> {
    pub(super) fn new(s: &'a [u16]) -> Self {
        Self {
            forward_offset: 0,
            back_offset: s.len(),
            iter: CharsUtf16::new(s),
        }
    }
}

impl Iterator for CharIndicesUtf16<'_> {
    type Item = (usize, char);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let result = self.iter.next();
        if let Some(c) = result {
            let offset = self.forward_offset;
            self.forward_offset += c.len_utf16();
            Some((offset, c))
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl FusedIterator for CharIndicesUtf16<'_> {}

impl DoubleEndedIterator for CharIndicesUtf16<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        let result = self.iter.next_back();
        if let Some(c) = result {
            self.back_offset -= c.len_utf16();
            Some((self.back_offset, c))
        } else {
            None
        }
    }
}

/// An iterator over the [`char`]s of a string slice, and their positions
///
/// This struct is created by the [`char_indices`][crate::Utf32Str::char_indices] method on
/// [`Utf32Str`][crate::Utf32Str]. See its documentation for more.
#[derive(Debug, Clone)]
pub struct CharIndicesUtf32<'a> {
    forward_offset: usize,
    back_offset: usize,
    iter: CharsUtf32<'a>,
}

impl CharIndicesUtf32<'_> {
    /// Returns the position of the next character, or the length of the underlying string if
    /// there are no more characters.
    #[inline]
    pub fn offset(&self) -> usize {
        self.forward_offset
    }
}

impl<'a> CharIndicesUtf32<'a> {
    pub(super) fn new(s: &'a [u32]) -> Self {
        Self {
            forward_offset: 0,
            back_offset: s.len(),
            iter: CharsUtf32::new(s),
        }
    }
}

impl Iterator for CharIndicesUtf32<'_> {
    type Item = (usize, char);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let result = self.iter.next();
        if let Some(c) = result {
            let offset = self.forward_offset;
            self.forward_offset += 1;
            Some((offset, c))
        } else {
            None
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl FusedIterator for CharIndicesUtf32<'_> {}

impl DoubleEndedIterator for CharIndicesUtf32<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        let result = self.iter.next_back();
        if let Some(c) = result {
            self.back_offset -= 1;
            Some((self.back_offset, c))
        } else {
            None
        }
    }
}

impl ExactSizeIterator for CharIndicesUtf32<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}

/// The return type of [`Utf16Str::escape_debug`][crate::Utf16Str::escape_debug].
#[derive(Debug, Clone)]
pub struct EscapeDebug<I> {
    iter: FlatMap<I, core::char::EscapeDebug, fn(char) -> core::char::EscapeDebug>,
}

impl<'a> EscapeDebug<CharsUtf16<'a>> {
    pub(super) fn new(s: &'a [u16]) -> Self {
        Self {
            iter: CharsUtf16::new(s).flat_map(|c| c.escape_debug()),
        }
    }
}

impl<'a> EscapeDebug<CharsUtf32<'a>> {
    pub(super) fn new(s: &'a [u32]) -> Self {
        Self {
            iter: CharsUtf32::new(s).flat_map(|c| c.escape_debug()),
        }
    }
}

/// The return type of [`Utf16Str::escape_default`][crate::Utf16Str::escape_default].
#[derive(Debug, Clone)]
pub struct EscapeDefault<I> {
    iter: FlatMap<I, core::char::EscapeDefault, fn(char) -> core::char::EscapeDefault>,
}

impl<'a> EscapeDefault<CharsUtf16<'a>> {
    pub(super) fn new(s: &'a [u16]) -> Self {
        Self {
            iter: CharsUtf16::new(s).flat_map(|c| c.escape_default()),
        }
    }
}

impl<'a> EscapeDefault<CharsUtf32<'a>> {
    pub(super) fn new(s: &'a [u32]) -> Self {
        Self {
            iter: CharsUtf32::new(s).flat_map(|c| c.escape_default()),
        }
    }
}

/// The return type of [`Utf16Str::escape_unicode`][crate::Utf16Str::escape_unicode].
#[derive(Debug, Clone)]
pub struct EscapeUnicode<I> {
    iter: FlatMap<I, core::char::EscapeUnicode, fn(char) -> core::char::EscapeUnicode>,
}

impl<'a> EscapeUnicode<CharsUtf16<'a>> {
    pub(super) fn new(s: &'a [u16]) -> Self {
        Self {
            iter: CharsUtf16::new(s).flat_map(|c| c.escape_unicode()),
        }
    }
}

impl<'a> EscapeUnicode<CharsUtf32<'a>> {
    pub(super) fn new(s: &'a [u32]) -> Self {
        Self {
            iter: CharsUtf32::new(s).flat_map(|c| c.escape_unicode()),
        }
    }
}

macro_rules! escape_impls {
    ($($name:ident),+) => {$(
        impl<I> core::fmt::Display for $name<I> where I: Iterator<Item = char> + Clone {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                self.clone().try_for_each(|c| f.write_char(c))
            }
        }

        impl< I> Iterator for $name<I> where I: Iterator<Item = char> {
            type Item = char;

            #[inline]
            fn next(&mut self) -> Option<Self::Item> {
                self.iter.next()
            }

            #[inline]
            fn size_hint(&self) -> (usize, Option<usize>) {
                let (lower, upper) = self.iter.size_hint();
                // Worst case, every char has to be unicode escaped as \u{NNNNNN}
                (lower, upper.and_then(|len| len.checked_mul(10)))
            }
        }

        impl<I> FusedIterator for $name<I> where I: Iterator<Item = char> + FusedIterator {}
    )+}
}

escape_impls!(EscapeDebug, EscapeDefault, EscapeUnicode);

/// An iterator over the [`u16`] code units of a UTF-16 string slice
///
/// This struct is created by the [`code_units`][crate::Utf16Str::code_units] method on
/// [`Utf16Str`][crate::Utf16Str]. See its documentation for more.
#[derive(Debug, Clone)]
pub struct CodeUnits<'a> {
    iter: Copied<Iter<'a, u16>>,
}

impl<'a> CodeUnits<'a> {
    pub(super) fn new(s: &'a [u16]) -> Self {
        Self {
            iter: s.iter().copied(),
        }
    }
}

impl Iterator for CodeUnits<'_> {
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl FusedIterator for CodeUnits<'_> {}

impl DoubleEndedIterator for CodeUnits<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.iter.next_back()
    }
}

impl ExactSizeIterator for CodeUnits<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.iter.len()
    }
}

/// An iterator over the lines of a [`crate::Utf16Str`], [`crate::Utf32Str`], or other wide string
/// that has the char_indices method. Returns string slices.
///
/// This struct is created with one of:
/// 1. The [`lines`][crate::Utf16Str::lines] method on [`crate::Utf16Str`]
/// 2. The [`lines`][crate::Utf32Str::lines] method on [`crate::Utf32Str`]
/// 3. etc.
///
/// See their documentation for more.
#[derive(Debug, Clone)]
pub struct Lines<'a, Str, CharIndices>
where
    Str: Borrow<Str> + Index<Range<usize>, Output = Str> + ?Sized,
    CharIndices: IntoIterator<Item = (usize, char)>,
{
    str: &'a Str,
    str_len: usize,
    char_indices: Peekable<CharIndices::IntoIter>,
}

impl<'a, Str, CharIndices> Lines<'a, Str, CharIndices>
where
    Str: Borrow<Str> + Index<Range<usize>, Output = Str> + ?Sized,
    CharIndices: IntoIterator<Item = (usize, char)>,
{
    pub(crate) fn new(str: &'a Str, str_len: usize, char_indices: CharIndices) -> Self {
        Self {
            str,
            str_len,
            char_indices: char_indices.into_iter().peekable(),
        }
    }
}

impl<'a, Str, CharIndices> Iterator for Lines<'a, Str, CharIndices>
where
    Str: Borrow<Str> + Index<Range<usize>, Output = Str> + ?Sized,
    CharIndices: IntoIterator<Item = (usize, char)>,
{
    type Item = &'a Str;

    fn next(&mut self) -> Option<Self::Item> {
        let mut current_char_index = self.char_indices.next()?;

        let line_start = current_char_index.0;
        let mut line_end = current_char_index.0;
        let mut previous_was_carriage_return;

        loop {
            if current_char_index.1 == '\n' {
                break;
            }

            if current_char_index.1 == '\r' {
                line_end = current_char_index.0;
                previous_was_carriage_return = true;
            } else {
                line_end = self
                    .char_indices
                    .peek()
                    .map(|ch_index| ch_index.0)
                    .unwrap_or(self.str_len);
                previous_was_carriage_return = false;
            }

            if let Some(current) = self.char_indices.next() {
                current_char_index = current;
            } else {
                line_end = if previous_was_carriage_return {
                    self.str_len
                } else {
                    line_end
                };
                break;
            }
        }

        Some(&self.str[line_start..line_end])
    }
}

// Since CharIndicesUtf16 is a FusedIterator, so is Lines
impl<Str, CharIndices> FusedIterator for Lines<'_, Str, CharIndices>
where
    Str: Borrow<Str> + Index<Range<usize>, Output = Str>,
    CharIndices: IntoIterator<Item = (usize, char)>,
{
}
