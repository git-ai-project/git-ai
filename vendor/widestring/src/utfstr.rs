//! UTF string slices.
//!
//! This module contains UTF string slices and related types.

use crate::{
    error::{Utf16Error, Utf32Error},
    is_utf16_low_surrogate,
    iter::{EncodeUtf16, EncodeUtf32, EncodeUtf8},
    validate_utf16, validate_utf32, U16Str, U32Str,
};
#[cfg(feature = "alloc")]
use crate::{Utf16String, Utf32String};
#[cfg(feature = "alloc")]
#[allow(unused_imports)]
use alloc::{borrow::Cow, boxed::Box, string::String};
#[allow(unused_imports)]
use core::{
    convert::{AsMut, AsRef, TryFrom},
    fmt::Write,
    ops::{Index, IndexMut, RangeBounds},
    slice::SliceIndex,
};

mod iter;

pub use iter::*;

#[inline]
const fn char_len_utf16(c: char) -> usize {
    c.len_utf16()
}

#[inline]
const fn char_len_utf32(_c: char) -> usize {
    1
}

macro_rules! utfstr_common_impl {
    {
        $(#[$utfstr_meta:meta])*
        struct $utfstr:ident([$uchar:ty]);
        type UtfString = $utfstring:ident;
        type UStr = $ustr:ident;
        type UCStr = $ucstr:ident;
        type UtfError = $utferror:ident;
        char_len_fn = $char_len_fn:ident;
        $(#[$from_slice_unchecked_meta:meta])*
        fn from_slice_unchecked() -> {}
        $(#[$from_slice_unchecked_mut_meta:meta])*
        fn from_slice_unchecked_mut() -> {}
        $(#[$from_boxed_slice_unchecked_meta:meta])*
        fn from_boxed_slice_unchecked() -> {}
        $(#[$get_unchecked_meta:meta])*
        fn get_unchecked() -> {}
        $(#[$get_unchecked_mut_meta:meta])*
        fn get_unchecked_mut() -> {}
        $(#[$len_meta:meta])*
        fn len() -> {}
    } => {
        $(#[$utfstr_meta])*
        #[allow(clippy::derive_hash_xor_eq)]
        #[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $utfstr {
            pub(crate) inner: [$uchar],
        }

        impl $utfstr {
            $(#[$from_slice_unchecked_meta])*
            #[allow(trivial_casts)]
            #[inline]
            #[must_use]
            pub const unsafe fn from_slice_unchecked(s: &[$uchar]) -> &Self {
                &*(s as *const [$uchar] as *const Self)
            }

            $(#[$from_slice_unchecked_mut_meta])*
            #[allow(trivial_casts)]
            #[inline]
            #[must_use]
            pub unsafe fn from_slice_unchecked_mut(s: &mut [$uchar]) -> &mut Self {
                &mut *(s as *mut [$uchar] as *mut Self)
            }

            $(#[$from_boxed_slice_unchecked_meta])*
            #[inline]
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub unsafe fn from_boxed_slice_unchecked(s: Box<[$uchar]>) -> Box<Self> {
                Box::from_raw(Box::into_raw(s) as *mut Self)
            }

            $(#[$get_unchecked_meta])*
            #[inline]
            #[must_use]
            pub unsafe fn get_unchecked<I>(&self, index: I) -> &Self
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                Self::from_slice_unchecked(self.inner.get_unchecked(index))
            }

            $(#[$get_unchecked_mut_meta])*
            #[inline]
            #[must_use]
            pub unsafe fn get_unchecked_mut<I>(&mut self, index: I) -> &mut Self
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                Self::from_slice_unchecked_mut(self.inner.get_unchecked_mut(index))
            }

            $(#[$len_meta])*
            #[inline]
            #[must_use]
            pub const fn len(&self) -> usize {
                self.inner.len()
            }

            /// Returns `true` if the string has a length of zero.
            #[inline]
            #[must_use]
            pub const fn is_empty(&self) -> bool {
                self.inner.is_empty()
            }

            /// Converts a string to a slice of its underlying elements.
            ///
            /// To convert the slice back into a string slice, use the
            /// [`from_slice`][Self::from_slice] function.
            #[inline]
            #[must_use]
            pub const fn as_slice(&self) -> &[$uchar] {
                &self.inner
            }

            /// Converts a mutable string to a mutable slice of its underlying elements.
            ///
            /// # Safety
            ///
            /// This function is unsafe because you can violate the invariants of this type when
            /// mutating the slice. The caller must ensure that the contents of the slice is valid
            /// UTF before the borrow ends and the underlying string is used.
            ///
            /// Use of this string type whose contents have been mutated to invalid UTF is
            /// undefined behavior.
            #[inline]
            #[must_use]
            pub unsafe fn as_mut_slice(&mut self) -> &mut [$uchar] {
                &mut self.inner
            }

            /// Converts a string slice to a raw pointer.
            ///
            /// This pointer will be pointing to the first element of the string slice.
            ///
            /// The caller must ensure that the returned pointer is never written to. If you need to
            /// mutate the contents of the string slice, use [`as_mut_ptr`][Self::as_mut_ptr].
            #[inline]
            #[must_use]
            pub const fn as_ptr(&self) -> *const $uchar {
                self.inner.as_ptr()
            }

            /// Converts a mutable string slice to a mutable pointer.
            ///
            /// This pointer will be pointing to the first element of the string slice.
            #[inline]
            #[must_use]
            pub fn as_mut_ptr(&mut self) -> *mut $uchar {
                self.inner.as_mut_ptr()
            }

            /// Returns this string as a wide string slice of undefined encoding.
            #[inline]
            #[must_use]
            pub const fn as_ustr(&self) -> &$ustr {
                $ustr::from_slice(self.as_slice())
            }

            /// Returns a string slice with leading and trailing whitespace removed.
            ///
            /// 'Whitespace' is defined according to the terms of the Unicode Derived Core Property
            /// `White_Space`.
            #[must_use]
            pub fn trim(&self) -> &Self {
                self.trim_start().trim_end()
            }

            /// Returns a string slice with leading whitespace removed.
            ///
            /// 'Whitespace' is defined according to the terms of the Unicode Derived Core Property
            /// `White_Space`.
            ///
            /// # Text directionality
            ///
            /// A string is a sequence of elements. `start` in this context means the first position
            /// of that sequence; for a left-to-right language like English or Russian, this will be
            /// left side, and for right-to-left languages like Arabic or Hebrew, this will be the
            /// right side.
            #[must_use]
            pub fn trim_start(&self) -> &Self {
                if let Some((index, _)) = self.char_indices().find(|(_, c)| !c.is_whitespace()) {
                    &self[index..]
                } else {
                    <&Self as Default>::default()
                }
            }

            /// Returns a string slice with trailing whitespace removed.
            ///
            /// 'Whitespace' is defined according to the terms of the Unicode Derived Core Property
            /// `White_Space`.
            ///
            /// # Text directionality
            ///
            /// A string is a sequence of elements. `end` in this context means the last position of
            /// that sequence; for a left-to-right language like English or Russian, this will be
            /// right side, and for right-to-left languages like Arabic or Hebrew, this will be the
            /// left side.
            #[must_use]
            pub fn trim_end(&self) -> &Self {
                if let Some((index, c)) = self.char_indices().rfind(|(_, c)| !c.is_whitespace()) {
                    &self[..index + $char_len_fn(c)]
                } else {
                    <&Self as Default>::default()
                }
            }

            /// Converts a boxed string into a boxed slice without copying or allocating.
            #[inline]
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn into_boxed_slice(self: Box<Self>) -> Box<[$uchar]> {
                // SAFETY: from_raw pointer is from into_raw
                unsafe { Box::from_raw(Box::into_raw(self) as *mut [$uchar]) }
            }

            /// Converts a boxed string slice into an owned UTF string without copying or
            /// allocating.
            #[inline]
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn into_utfstring(self: Box<Self>) -> $utfstring {
                unsafe { $utfstring::from_vec_unchecked(self.into_boxed_slice().into_vec()) }
            }

            /// Creates a new owned string by repeating this string `n` times.
            ///
            /// # Panics
            ///
            /// This function will panic if the capacity would overflow.
            #[inline]
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn repeat(&self, n: usize) -> $utfstring {
                unsafe { $utfstring::from_vec_unchecked(self.as_slice().repeat(n)) }
            }
        }

        impl AsMut<$utfstr> for $utfstr {
            #[inline]
            fn as_mut(&mut self) -> &mut $utfstr {
                self
            }
        }

        impl AsRef<$utfstr> for $utfstr {
            #[inline]
            fn as_ref(&self) -> &$utfstr {
                self
            }
        }

        impl AsRef<[$uchar]> for $utfstr {
            #[inline]
            fn as_ref(&self) -> &[$uchar] {
                self.as_slice()
            }
        }

        impl AsRef<$ustr> for $utfstr {
            #[inline]
            fn as_ref(&self) -> &$ustr {
                self.as_ustr()
            }
        }

        impl core::fmt::Debug for $utfstr {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_char('"')?;
                self.escape_debug().try_for_each(|c| f.write_char(c))?;
                f.write_char('"')
            }
        }

        impl Default for &$utfstr {
            #[inline]
            fn default() -> Self {
                // SAFETY: Empty slice is always valid
                unsafe { $utfstr::from_slice_unchecked(&[]) }
            }
        }

        impl Default for &mut $utfstr {
            #[inline]
            fn default() -> Self {
                // SAFETY: Empty slice is valways valid
                unsafe { $utfstr::from_slice_unchecked_mut(&mut []) }
            }
        }

        impl core::fmt::Display for $utfstr {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                self.chars().try_for_each(|c| f.write_char(c))
            }
        }

        #[cfg(feature = "alloc")]
        impl From<Box<$utfstr>> for Box<[$uchar]> {
            #[inline]
            fn from(value: Box<$utfstr>) -> Self {
                value.into_boxed_slice()
            }
        }

        impl<'a> From<&'a $utfstr> for &'a $ustr {
            #[inline]
            fn from(value: &'a $utfstr) -> Self {
                value.as_ustr()
            }
        }

        impl<'a> From<&'a $utfstr> for &'a [$uchar] {
            #[inline]
            fn from(value: &'a $utfstr) -> Self {
                value.as_slice()
            }
        }

        #[cfg(feature = "std")]
        impl From<&$utfstr> for std::ffi::OsString {
            #[inline]
            fn from(value: &$utfstr) -> std::ffi::OsString {
                value.as_ustr().to_os_string()
            }
        }

        impl PartialEq<$utfstr> for &$utfstr {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        #[cfg(feature = "alloc")]
        impl<'a, 'b> PartialEq<Cow<'a, $utfstr>> for &'b $utfstr {
            #[inline]
            fn eq(&self, other: &Cow<'a, $utfstr>) -> bool {
                self == other.as_ref()
            }
        }

        #[cfg(feature = "alloc")]
        impl PartialEq<$utfstr> for Cow<'_, $utfstr> {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_ref() == other
            }
        }

        #[cfg(feature = "alloc")]
        impl<'a, 'b> PartialEq<&'a $utfstr> for Cow<'b, $utfstr> {
            #[inline]
            fn eq(&self, other: &&'a $utfstr) -> bool {
                self.as_ref() == *other
            }
        }

        impl PartialEq<$ustr> for $utfstr {
            #[inline]
            fn eq(&self, other: &$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstr> for $ustr {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ucstr> for $utfstr {
            #[inline]
            fn eq(&self, other: &crate::$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstr> for crate::$ucstr {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<str> for $utfstr {
            #[inline]
            fn eq(&self, other: &str) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<&str> for $utfstr {
            #[inline]
            fn eq(&self, other: &&str) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<str> for &$utfstr {
            #[inline]
            fn eq(&self, other: &str) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<$utfstr> for str {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<$utfstr> for &str {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.chars().eq(other.chars())
            }
        }

        #[cfg(feature = "alloc")]
        impl<'a, 'b> PartialEq<Cow<'a, str>> for &'b $utfstr {
            #[inline]
            fn eq(&self, other: &Cow<'a, str>) -> bool {
                self == other.as_ref()
            }
        }

        #[cfg(feature = "alloc")]
        impl PartialEq<$utfstr> for Cow<'_, str> {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_ref() == other
            }
        }

        #[cfg(feature = "alloc")]
        impl<'a, 'b> PartialEq<&'a $utfstr> for Cow<'b, str> {
            #[inline]
            fn eq(&self, other: &&'a $utfstr) -> bool {
                self.as_ref() == *other
            }
        }

        impl<'a> TryFrom<&'a $ustr> for &'a $utfstr {
            type Error = $utferror;

            #[inline]
            fn try_from(value: &'a $ustr) -> Result<Self, Self::Error> {
                $utfstr::from_ustr(value)
            }
        }

        impl<'a> TryFrom<&'a crate::$ucstr> for &'a $utfstr {
            type Error = $utferror;

            #[inline]
            fn try_from(value: &'a crate::$ucstr) -> Result<Self, Self::Error> {
                $utfstr::from_ucstr(value)
            }
        }
    };
}

utfstr_common_impl! {
    /// UTF-16 string slice for [`Utf16String`][crate::Utf16String].
    ///
    /// [`Utf16Str`] is to [`Utf16String`][crate::Utf16String] as [`str`] is to [`String`].
    ///
    /// [`Utf16Str`] slices are string slices that are always valid UTF-16 encoding. This is unlike
    /// the [`U16Str`][U16Str] string slices, which may not have valid encoding. In this way,
    /// [`Utf16Str`] string slices most resemble native [`str`] slices of all the types in this
    /// crate.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`Utf16Str`] is with the [`utf16str!`][crate::utf16str] macro to
    /// convert string literals into string slices at compile time:
    ///
    /// ```
    /// use widestring::utf16str;
    /// let hello = utf16str!("Hello, world!");
    /// ```
    ///
    /// You can also convert a [`u16`] slice directly, provided it is valid UTF-16:
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let sparkle_heart = [0xd83d, 0xdc96];
    /// let sparkle_heart = Utf16Str::from_slice(&sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    struct Utf16Str([u16]);

    type UtfString = Utf16String;
    type UStr = U16Str;
    type UCStr = U16CStr;
    type UtfError = Utf16Error;
    char_len_fn = char_len_utf16;

    /// Converts a slice to a string slice without checking that the string contains valid UTF-16.
    ///
    /// See the safe version, [`from_slice`][Self::from_slice], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the slice passed to it is valid
    /// UTF-16. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf16Str`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = unsafe { Utf16Str::from_slice_unchecked(&sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    fn from_slice_unchecked() -> {}

    /// Converts a mutable slice to a mutable string slice without checking that the string contains
    /// valid UTF-16.
    ///
    /// See the safe version, [`from_slice_mut`][Self::from_slice_mut], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the slice passed to it is valid
    /// UTF-16. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf16Str`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let mut sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = unsafe { Utf16Str::from_slice_unchecked_mut(&mut sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    fn from_slice_unchecked_mut() -> {}

    /// Converts a boxed slice to a boxed string slice without checking that the string contains
    /// valid UTF-16.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check if the string slice is valid UTF-16, and
    /// [`Utf16Str`] must always be valid UTF-16.
    fn from_boxed_slice_unchecked() -> {}

    /// Returns an unchecked subslice of this string slice.
    ///
    /// This is the unchecked alternative to indexing the string slice.
    ///
    /// # Safety
    ///
    /// Callers of this function are responsible that these preconditions are satisfied:
    ///
    /// - The starting index must not exceed the ending index;
    /// - Indexes must be within bounds of the original slice;
    /// - Indexes must lie on UTF-16 sequence boundaries.
    ///
    /// Failing that, the returned string slice may reference invalid memory or violate the
    /// invariants communicated by the type.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::{utf16str};
    /// let v = utf16str!("⚧️🏳️‍⚧️➡️s");
    /// unsafe {
    ///     assert_eq!(utf16str!("⚧️"), v.get_unchecked(..2));
    ///     assert_eq!(utf16str!("🏳️‍⚧️"), v.get_unchecked(2..8));
    ///     assert_eq!(utf16str!("➡️"), v.get_unchecked(8..10));
    ///     assert_eq!(utf16str!("s"), v.get_unchecked(10..));
    /// }
    /// ```
    fn get_unchecked() -> {}

    /// Returns a mutable, unchecked subslice of this string slice
    ///
    /// This is the unchecked alternative to indexing the string slice.
    ///
    /// # Safety
    ///
    /// Callers of this function are responsible that these preconditions are satisfied:
    ///
    /// - The starting index must not exceed the ending index;
    /// - Indexes must be within bounds of the original slice;
    /// - Indexes must lie on UTF-16 sequence boundaries.
    ///
    /// Failing that, the returned string slice may reference invalid memory or violate the
    /// invariants communicated by the type.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::{utf16str};
    /// # #[cfg(feature = "alloc")] {
    /// let mut v = utf16str!("⚧️🏳️‍⚧️➡️s").to_owned();
    /// unsafe {
    ///     assert_eq!(utf16str!("⚧️"), v.get_unchecked_mut(..2));
    ///     assert_eq!(utf16str!("🏳️‍⚧️"), v.get_unchecked_mut(2..8));
    ///     assert_eq!(utf16str!("➡️"), v.get_unchecked_mut(8..10));
    ///     assert_eq!(utf16str!("s"), v.get_unchecked_mut(10..));
    /// }
    /// # }
    /// ```
    fn get_unchecked_mut() -> {}

    /// Returns the length of `self`.
    ///
    /// This length is in `u16` values, not [`char`]s or graphemes. In other words, it may not be
    /// what human considers the length of the string.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// assert_eq!(utf16str!("foo").len(), 3);
    ///
    /// let complex = utf16str!("⚧️🏳️‍⚧️➡️s");
    /// assert_eq!(complex.len(), 11);
    /// assert_eq!(complex.chars().count(), 10);
    /// ```
    fn len() -> {}
}

utfstr_common_impl! {
    /// UTF-32 string slice for [`Utf32String`][crate::Utf32String].
    ///
    /// [`Utf32Str`] is to [`Utf32String`][crate::Utf32String] as [`str`] is to [`String`].
    ///
    /// [`Utf32Str`] slices are string slices that are always valid UTF-32 encoding. This is unlike
    /// the [`U32Str`][crate::U16Str] string slices, which may not have valid encoding. In this way,
    /// [`Utf32Str`] string slices most resemble native [`str`] slices of all the types in this
    /// crate.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`Utf32Str`] is with the [`utf32str!`][crate::utf32str] macro to
    /// convert string literals into string slices at compile time:
    ///
    /// ```
    /// use widestring::utf32str;
    /// let hello = utf32str!("Hello, world!");
    /// ```
    ///
    /// You can also convert a [`u32`] slice directly, provided it is valid UTF-32:
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let sparkle_heart = [0x1f496];
    /// let sparkle_heart = Utf32Str::from_slice(&sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// Since [`char`] slices are valid UTF-32, a slice of [`char`]s can be easily converted to a
    /// string slice:
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let sparkle_heart = ['💖'; 3];
    /// let sparkle_heart = Utf32Str::from_char_slice(&sparkle_heart);
    ///
    /// assert_eq!("💖💖💖", sparkle_heart);
    /// ```
    struct Utf32Str([u32]);

    type UtfString = Utf32String;
    type UStr = U32Str;
    type UCStr = U32CStr;
    type UtfError = Utf32Error;
    char_len_fn = char_len_utf32;

    /// Converts a slice to a string slice without checking that the string contains valid UTF-32.
    ///
    /// See the safe version, [`from_slice`][Self::from_slice], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the slice passed to it is valid
    /// UTF-32. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf32Str`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = unsafe { Utf32Str::from_slice_unchecked(&sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    fn from_slice_unchecked() -> {}

    /// Converts a mutable slice to a mutable string slice without checking that the string contains
    /// valid UTF-32.
    ///
    /// See the safe version, [`from_slice_mut`][Self::from_slice_mut], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the slice passed to it is valid
    /// UTF-32. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf32Str`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let mut sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = unsafe { Utf32Str::from_slice_unchecked_mut(&mut sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    fn from_slice_unchecked_mut() -> {}

    /// Converts a boxed slice to a boxed string slice without checking that the string contains
    /// valid UTF-32.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check if the string slice is valid UTF-32, and
    /// [`Utf32Str`] must always be valid UTF-32.
    fn from_boxed_slice_unchecked() -> {}

    /// Returns an unchecked subslice of this string slice.
    ///
    /// This is the unchecked alternative to indexing the string slice.
    ///
    /// # Safety
    ///
    /// Callers of this function are responsible that these preconditions are satisfied:
    ///
    /// - The starting index must not exceed the ending index;
    /// - Indexes must be within bounds of the original slice;
    ///
    /// Failing that, the returned string slice may reference invalid memory or violate the
    /// invariants communicated by the type.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// let v = utf32str!("⚧️🏳️‍⚧️➡️s");
    /// unsafe {
    ///     assert_eq!(utf32str!("⚧️"), v.get_unchecked(..2));
    ///     assert_eq!(utf32str!("🏳️‍⚧️"), v.get_unchecked(2..7));
    ///     assert_eq!(utf32str!("➡️"), v.get_unchecked(7..9));
    ///     assert_eq!(utf32str!("s"), v.get_unchecked(9..))
    /// }
    /// ```
    fn get_unchecked() -> {}

    /// Returns a mutable, unchecked subslice of this string slice
    ///
    /// This is the unchecked alternative to indexing the string slice.
    ///
    /// # Safety
    ///
    /// Callers of this function are responsible that these preconditions are satisfied:
    ///
    /// - The starting index must not exceed the ending index;
    /// - Indexes must be within bounds of the original slice;
    ///
    /// Failing that, the returned string slice may reference invalid memory or violate the
    /// invariants communicated by the type.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// # #[cfg(feature = "alloc")] {
    /// let mut v = utf32str!("⚧️🏳️‍⚧️➡️s").to_owned();
    /// unsafe {
    ///     assert_eq!(utf32str!("⚧️"), v.get_unchecked_mut(..2));
    ///     assert_eq!(utf32str!("🏳️‍⚧️"), v.get_unchecked_mut(2..7));
    ///     assert_eq!(utf32str!("➡️"), v.get_unchecked_mut(7..9));
    ///     assert_eq!(utf32str!("s"), v.get_unchecked_mut(9..))
    /// }
    /// # }
    /// ```
    fn get_unchecked_mut() -> {}

    /// Returns the length of `self`.
    ///
    /// This length is in the number of [`char`]s in the slice, not graphemes. In other words, it
    /// may not be what human considers the length of the string.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// assert_eq!(utf32str!("foo").len(), 3);
    ///
    /// let complex = utf32str!("⚧️🏳️‍⚧️➡️s");
    /// assert_eq!(complex.len(), 10);
    /// assert_eq!(complex.chars().count(), 10);
    /// ```
    fn len() -> {}
}

impl Utf16Str {
    /// Converts a slice of UTF-16 data to a string slice.
    ///
    /// Not all slices of [`u16`] values are valid to convert, since [`Utf16Str`] requires that it
    /// is always valid UTF-16. This function checks to ensure that the values are valid UTF-16, and
    /// then does the conversion.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_slice_unchecked`][Self::from_slice_unchecked], which has the same behavior but skips
    /// the check.
    ///
    /// If you need an owned string, consider using [`Utf16String::from_vec`] instead.
    ///
    /// Because you can stack-allocate a `[u16; N]`, this function is one way to have a
    /// stack-allocated string. Indeed, the [`utf16str!`][crate::utf16str] macro does exactly this
    /// after converting from UTF-8 to UTF-16.
    ///
    /// # Errors
    ///
    /// Returns an error if the slice is not UTF-16 with a description as to why the provided slice
    /// is not UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = Utf16Str::from_slice(&sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    ///
    /// assert!(Utf16Str::from_slice(&sparkle_heart).is_err());
    /// ```
    pub fn from_slice(s: &[u16]) -> Result<&Self, Utf16Error> {
        validate_utf16(s)?;
        // SAFETY: Just validated
        Ok(unsafe { Self::from_slice_unchecked(s) })
    }

    /// Converts a mutable slice of UTF-16 data to a mutable string slice.
    ///
    /// Not all slices of [`u16`] values are valid to convert, since [`Utf16Str`] requires that it
    /// is always valid UTF-16. This function checks to ensure that the values are valid UTF-16, and
    /// then does the conversion.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_slice_unchecked_mut`][Self::from_slice_unchecked_mut], which has the same behavior
    /// but skips the check.
    ///
    /// If you need an owned string, consider using [`Utf16String::from_vec`] instead.
    ///
    /// Because you can stack-allocate a `[u16; N]`, this function is one way to have a
    /// stack-allocated string. Indeed, the [`utf16str!`][crate::utf16str] macro does exactly this
    /// after converting from UTF-8 to UTF-16.
    ///
    /// # Errors
    ///
    /// Returns an error if the slice is not UTF-16 with a description as to why the provided slice
    /// is not UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let mut sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = Utf16Str::from_slice_mut(&mut sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::Utf16Str;
    ///
    /// let mut sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    ///
    /// assert!(Utf16Str::from_slice_mut(&mut sparkle_heart).is_err());
    /// ```
    pub fn from_slice_mut(s: &mut [u16]) -> Result<&mut Self, Utf16Error> {
        validate_utf16(s)?;
        // SAFETY: Just validated
        Ok(unsafe { Self::from_slice_unchecked_mut(s) })
    }

    /// Converts a wide string slice of undefined encoding to a UTF-16 string slice without checking
    /// if the string slice is valid UTF-16.
    ///
    /// See the safe version, [`from_ustr`][Self::from_ustr], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-16. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf16Str`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf16Str, u16str};
    ///
    /// let sparkle_heart = u16str!("💖");
    /// let sparkle_heart = unsafe { Utf16Str::from_ustr_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[must_use]
    pub const unsafe fn from_ustr_unchecked(s: &U16Str) -> &Self {
        Self::from_slice_unchecked(s.as_slice())
    }

    /// Converts a mutable wide string slice of undefined encoding to a mutable UTF-16 string slice
    /// without checking if the string slice is valid UTF-16.
    ///
    /// See the safe version, [`from_ustr_mut`][Self::from_ustr_mut], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-16. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf16Str`] is always valid UTF-16.
    #[must_use]
    pub unsafe fn from_ustr_unchecked_mut(s: &mut U16Str) -> &mut Self {
        Self::from_slice_unchecked_mut(s.as_mut_slice())
    }

    /// Converts a wide string slice of undefined encoding to a UTF-16 string slice.
    ///
    /// Since [`U16Str`] does not have a specified encoding, this conversion may fail if the
    /// [`U16Str`] does not contain valid UTF-16 data.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ustr_unchecked`][Self::from_ustr_unchecked], which has the same behavior
    /// but skips the check.
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-16 with a description as to why the
    /// provided string slice is not UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf16Str, u16str};
    ///
    /// let sparkle_heart = u16str!("💖");
    /// let sparkle_heart = Utf16Str::from_ustr(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    pub fn from_ustr(s: &U16Str) -> Result<&Self, Utf16Error> {
        Self::from_slice(s.as_slice())
    }

    /// Converts a mutable wide string slice of undefined encoding to a mutable UTF-16 string slice.
    ///
    /// Since [`U16Str`] does not have a specified encoding, this conversion may fail if the
    /// [`U16Str`] does not contain valid UTF-16 data.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ustr_unchecked_mut`][Self::from_ustr_unchecked_mut], which has the same behavior
    /// but skips the check.
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-16 with a description as to why the
    /// provided string slice is not UTF-16.
    #[inline]
    pub fn from_ustr_mut(s: &mut U16Str) -> Result<&mut Self, Utf16Error> {
        Self::from_slice_mut(s.as_mut_slice())
    }

    /// Converts a wide C string slice to a UTF-16 string slice without checking if the
    /// string slice is valid UTF-16.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// See the safe version, [`from_ucstr`][Self::from_ucstr], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-16. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf16Str`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf16Str, u16cstr};
    ///
    /// let sparkle_heart = u16cstr!("💖");
    /// let sparkle_heart = unsafe { Utf16Str::from_ucstr_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_ucstr_unchecked(s: &crate::U16CStr) -> &Self {
        Self::from_slice_unchecked(s.as_slice())
    }

    /// Converts a mutable wide C string slice to a mutable UTF-16 string slice without
    /// checking if the string slice is valid UTF-16.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// See the safe version, [`from_ucstr_mut`][Self::from_ucstr_mut], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-16. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf16Str`] is always valid UTF-16.
    #[inline]
    #[must_use]
    pub unsafe fn from_ucstr_unchecked_mut(s: &mut crate::U16CStr) -> &mut Self {
        Self::from_slice_unchecked_mut(s.as_mut_slice())
    }

    /// Converts a wide C string slice to a UTF-16 string slice.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// Since [`U16CStr`][crate::U16CStr] does not have a specified encoding, this conversion may
    /// fail if the [`U16CStr`][crate::U16CStr] does not contain valid UTF-16 data.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ucstr_unchecked`][Self::from_ucstr_unchecked], which has the same behavior
    /// but skips the check.
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-16 with a description as to why the
    /// provided string slice is not UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf16Str, u16cstr};
    ///
    /// let sparkle_heart = u16cstr!("💖");
    /// let sparkle_heart = Utf16Str::from_ucstr(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    pub fn from_ucstr(s: &crate::U16CStr) -> Result<&Self, Utf16Error> {
        Self::from_slice(s.as_slice())
    }

    /// Converts a mutable wide C string slice to a mutable UTF-16 string slice.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// Since [`U16CStr`][crate::U16CStr] does not have a specified encoding, this conversion may
    /// fail if the [`U16CStr`][crate::U16CStr] does not contain valid UTF-16 data.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ucstr_unchecked_mut`][Self::from_ucstr_unchecked_mut], which has the same behavior
    /// but skips the check.
    ///
    /// # Safety
    ///
    /// This method is unsafe because you can violate the invariants of [`U16CStr`][crate::U16CStr]
    /// when mutating the slice (i.e. by adding interior nul values).
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-16 with a description as to why the
    /// provided string slice is not UTF-16.
    #[inline]
    pub unsafe fn from_ucstr_mut(s: &mut crate::U16CStr) -> Result<&mut Self, Utf16Error> {
        Self::from_slice_mut(s.as_mut_slice())
    }

    /// Converts to a standard UTF-8 [`String`].
    ///
    /// Because this string is always valid UTF-16, the conversion is lossless and non-fallible.
    #[inline]
    #[allow(clippy::inherent_to_string_shadow_display)]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_string(&self) -> String {
        String::from_utf16(self.as_slice()).unwrap()
    }

    /// Checks that `index`-th value is the value in a UTF-16 code point sequence or the end of the
    /// string.
    ///
    /// Returns `true` if the value at `index` is not a UTF-16 surrogate value, or if the value at
    /// `index` is the first value of a surrogate pair (the "high" surrogate). Returns `false` if
    /// the value at `index` is the second value of a surrogate pair (a.k.a the "low" surrogate).
    ///
    /// The start and end of the string (when `index == self.len()`) are considered to be
    /// boundaries.
    ///
    /// Returns `false` if `index is greater than `self.len()`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// let s = utf16str!("Sparkle 💖 Heart");
    /// assert!(s.is_char_boundary(0));
    ///
    /// // high surrogate of `💖`
    /// assert!(s.is_char_boundary(8));
    /// // low surrogate of `💖`
    /// assert!(!s.is_char_boundary(9));
    ///
    /// assert!(s.is_char_boundary(s.len()));
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_char_boundary(&self, index: usize) -> bool {
        if index > self.len() {
            false
        } else if index == self.len() {
            true
        } else {
            !is_utf16_low_surrogate(self.inner[index])
        }
    }

    /// Returns a subslice of this string.
    ///
    /// This is the non-panicking alternative to indexing the string. Returns [`None`] whenever
    /// equivalent indexing operation would panic.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::{utf16str};
    /// let v = utf16str!("⚧️🏳️‍⚧️➡️s");
    ///
    /// assert_eq!(Some(utf16str!("⚧️")), v.get(..2));
    /// assert_eq!(Some(utf16str!("🏳️‍⚧️")), v.get(2..8));
    /// assert_eq!(Some(utf16str!("➡️")), v.get(8..10));
    /// assert_eq!(Some(utf16str!("s")), v.get(10..));
    ///
    /// assert!(v.get(3..4).is_none());
    /// ```
    #[inline]
    #[must_use]
    pub fn get<I>(&self, index: I) -> Option<&Self>
    where
        I: RangeBounds<usize> + SliceIndex<[u16], Output = [u16]>,
    {
        // TODO: Use SliceIndex directly when it is stabilized
        let range = crate::range_check(index, ..self.len())?;
        if !self.is_char_boundary(range.start) || !self.is_char_boundary(range.end) {
            return None;
        }

        // SAFETY: range_check verified bounds, and we just verified char boundaries
        Some(unsafe { self.get_unchecked(range) })
    }

    /// Returns a mutable subslice of this string.
    ///
    /// This is the non-panicking alternative to indexing the string. Returns [`None`] whenever
    /// equivalent indexing operation would panic.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::{utf16str};
    /// # #[cfg(feature = "alloc")] {
    /// let mut v = utf16str!("⚧️🏳️‍⚧️➡️s").to_owned();
    ///
    /// assert_eq!(utf16str!("⚧️"), v.get_mut(..2).unwrap());
    /// assert_eq!(utf16str!("🏳️‍⚧️"), v.get_mut(2..8).unwrap());
    /// assert_eq!(utf16str!("➡️"), v.get_mut(8..10).unwrap());
    /// assert_eq!(utf16str!("s"), v.get_mut(10..).unwrap());
    ///
    /// assert!(v.get_mut(3..4).is_none());
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn get_mut<I>(&mut self, index: I) -> Option<&mut Self>
    where
        I: RangeBounds<usize> + SliceIndex<[u16], Output = [u16]>,
    {
        // TODO: Use SliceIndex directly when it is stabilized
        let range = crate::range_check(index, ..self.len())?;
        if !self.is_char_boundary(range.start) || !self.is_char_boundary(range.end) {
            return None;
        }

        // SAFETY: range_check verified bounds, and we just verified char boundaries
        Some(unsafe { self.get_unchecked_mut(range) })
    }

    /// Divide one string slice into two at an index.
    ///
    /// The argument, `mid`, should be an offset from the start of the string. It must also be on
    /// the boundary of a UTF-16 code point.
    ///
    /// The two slices returned go from the start of the string slice to `mid`, and from `mid` to
    /// the end of the string slice.
    ///
    /// To get mutable string slices instead, see the [`split_at_mut`][Self::split_at_mut] method.
    ///
    /// # Panics
    ///
    /// Panics if `mid` is not on a UTF-16 code point boundary, or if it is past the end of the last
    /// code point of the string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// let s = utf16str!("Per Martin-Löf");
    ///
    /// let (first, last) = s.split_at(3);
    ///
    /// assert_eq!("Per", first);
    /// assert_eq!(" Martin-Löf", last);
    /// ```
    #[inline]
    #[must_use]
    pub fn split_at(&self, mid: usize) -> (&Self, &Self) {
        assert!(self.is_char_boundary(mid));
        let (a, b) = self.inner.split_at(mid);
        unsafe { (Self::from_slice_unchecked(a), Self::from_slice_unchecked(b)) }
    }

    /// Divide one mutable string slice into two at an index.
    ///
    /// The argument, `mid`, should be an offset from the start of the string. It must also be on
    /// the boundary of a UTF-16 code point.
    ///
    /// The two slices returned go from the start of the string slice to `mid`, and from `mid` to
    /// the end of the string slice.
    ///
    /// To get immutable string slices instead, see the [`split_at`][Self::split_at] method.
    ///
    /// # Panics
    ///
    /// Panics if `mid` is not on a UTF-16 code point boundary, or if it is past the end of the last
    /// code point of the string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// # #[cfg(feature = "alloc")] {
    /// let mut s = utf16str!("Per Martin-Löf").to_owned();
    ///
    /// let (first, last) = s.split_at_mut(3);
    ///
    /// assert_eq!("Per", first);
    /// assert_eq!(" Martin-Löf", last);
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn split_at_mut(&mut self, mid: usize) -> (&mut Self, &mut Self) {
        assert!(self.is_char_boundary(mid));
        let (a, b) = self.inner.split_at_mut(mid);
        unsafe {
            (
                Self::from_slice_unchecked_mut(a),
                Self::from_slice_unchecked_mut(b),
            )
        }
    }

    /// Returns an iterator over the [`char`]s of a string slice.
    ///
    /// As this string slice consists of valid UTF-16, we can iterate through a string slice by
    /// [`char`]. This method returns such an iterator.
    ///
    /// It's important to remember that [`char`] represents a Unicode Scalar Value, and might not
    /// match your idea of what a 'character' is. Iteration over grapheme clusters may be what you
    /// actually want. This functionality is not provided by this crate.
    #[inline]
    #[must_use]
    pub fn chars(&self) -> CharsUtf16<'_> {
        CharsUtf16::new(self.as_slice())
    }

    /// Returns an iterator over the [`char`]s of a string slice and their positions.
    ///
    /// As this string slice consists of valid UTF-16, we can iterate through a string slice by
    /// [`char`]. This method returns an iterator of both these [`char`]s as well as their offsets.
    ///
    /// The iterator yields tuples. The position is first, the [`char`] is second.
    #[inline]
    #[must_use]
    pub fn char_indices(&self) -> CharIndicesUtf16<'_> {
        CharIndicesUtf16::new(self.as_slice())
    }

    /// An iterator over the [`u16`] code units of a string slice.
    ///
    /// As a UTF-16 string slice consists of a sequence of [`u16`] code units, we can iterate
    /// through a string slice by each code unit. This method returns such an iterator.
    #[must_use]
    pub fn code_units(&self) -> CodeUnits<'_> {
        CodeUnits::new(self.as_slice())
    }

    /// Returns an iterator of bytes over the string encoded as UTF-8.
    #[must_use]
    pub fn encode_utf8(&self) -> EncodeUtf8<CharsUtf16<'_>> {
        crate::encode_utf8(self.chars())
    }

    /// Returns an iterator of [`u32`] over the sting encoded as UTF-32.
    #[must_use]
    pub fn encode_utf32(&self) -> EncodeUtf32<CharsUtf16<'_>> {
        crate::encode_utf32(self.chars())
    }

    /// Returns an iterator that escapes each [`char`] in `self` with [`char::escape_debug`].
    #[inline]
    #[must_use]
    pub fn escape_debug(&self) -> EscapeDebug<CharsUtf16<'_>> {
        EscapeDebug::<CharsUtf16>::new(self.as_slice())
    }

    /// Returns an iterator that escapes each [`char`] in `self` with [`char::escape_default`].
    #[inline]
    #[must_use]
    pub fn escape_default(&self) -> EscapeDefault<CharsUtf16<'_>> {
        EscapeDefault::<CharsUtf16>::new(self.as_slice())
    }

    /// Returns an iterator that escapes each [`char`] in `self` with [`char::escape_unicode`].
    #[inline]
    #[must_use]
    pub fn escape_unicode(&self) -> EscapeUnicode<CharsUtf16<'_>> {
        EscapeUnicode::<CharsUtf16>::new(self.as_slice())
    }

    /// Returns the lowercase equivalent of this string slice, as a new [`Utf16String`].
    ///
    /// 'Lowercase' is defined according to the terms of the Unicode Derived Core Property
    /// `Lowercase`.
    ///
    /// Since some characters can expand into multiple characters when changing the case, this
    /// function returns a [`Utf16String`] instead of modifying the parameter in-place.
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_lowercase(&self) -> Utf16String {
        let mut s = Utf16String::with_capacity(self.len());
        for c in self.chars() {
            for lower in c.to_lowercase() {
                s.push(lower);
            }
        }
        s
    }

    /// Returns the uppercase equivalent of this string slice, as a new [`Utf16String`].
    ///
    /// 'Uppercase' is defined according to the terms of the Unicode Derived Core Property
    /// `Uppercase`.
    ///
    /// Since some characters can expand into multiple characters when changing the case, this
    /// function returns a [`Utf16String`] instead of modifying the parameter in-place.
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_uppercase(&self) -> Utf16String {
        let mut s = Utf16String::with_capacity(self.len());
        for c in self.chars() {
            for lower in c.to_uppercase() {
                s.push(lower);
            }
        }
        s
    }

    /// Returns an iterator over the lines of a [`Utf16Str`], as string slices.
    ///
    /// Lines are split at line endings that are either newlines (`\n`) or
    /// sequences of a carriage return followed by a line feed (`\r\n`).
    ///
    /// Line terminators are not included in the lines returned by the iterator.
    ///
    /// Note that any carriage return (`\r`) not immediately followed by a
    /// line feed (`\n`) does not split a line. These carriage returns are
    /// thereby included in the produced lines.
    ///
    /// The final line ending is optional. A string that ends with a final line
    /// ending will return the same lines as an otherwise identical string
    /// without a final line ending.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::utf16str;
    ///
    /// let text = utf16str!("foo\r\nbar\n\nbaz\r");
    /// let mut lines = text.lines();
    ///
    /// assert_eq!(Some(utf16str!("foo")), lines.next());
    /// assert_eq!(Some(utf16str!("bar")), lines.next());
    /// assert_eq!(Some(utf16str!("")), lines.next());
    /// // Trailing carriage return is included in the last line
    /// assert_eq!(Some(utf16str!("baz\r")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    ///
    /// The final line does not require any ending:
    ///
    /// ```
    /// use widestring::utf16str;
    ///
    /// let text = utf16str!("foo\nbar\n\r\nbaz");
    /// let mut lines = text.lines();
    ///
    /// assert_eq!(Some(utf16str!("foo")), lines.next());
    /// assert_eq!(Some(utf16str!("bar")), lines.next());
    /// assert_eq!(Some(utf16str!("")), lines.next());
    /// assert_eq!(Some(utf16str!("baz")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    pub fn lines(&self) -> Lines<'_, Utf16Str, CharIndicesUtf16<'_>> {
        Lines::new(self, self.len(), self.char_indices())
    }
}

impl Utf32Str {
    /// Converts a slice of UTF-32 data to a string slice.
    ///
    /// Not all slices of [`u32`] values are valid to convert, since [`Utf32Str`] requires that it
    /// is always valid UTF-32. This function checks to ensure that the values are valid UTF-32, and
    /// then does the conversion.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_slice_unchecked`][Self::from_slice_unchecked], which has the same behavior but skips
    /// the check.
    ///
    /// If you need an owned string, consider using [`Utf32String::from_vec`] instead.
    ///
    /// Because you can stack-allocate a `[u32; N]`, this function is one way to have a
    /// stack-allocated string. Indeed, the [`utf32str!`][crate::utf32str] macro does exactly this
    /// after converting from UTF-8 to UTF-32.
    ///
    /// # Errors
    ///
    /// Returns an error if the slice is not UTF-32 with a description as to why the provided slice
    /// is not UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = Utf32Str::from_slice(&sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    ///
    /// assert!(Utf32Str::from_slice(&sparkle_heart).is_err());
    /// ```
    pub fn from_slice(s: &[u32]) -> Result<&Self, Utf32Error> {
        validate_utf32(s)?;
        // SAFETY: Just validated
        Ok(unsafe { Self::from_slice_unchecked(s) })
    }

    /// Converts a mutable slice of UTF-32 data to a mutable string slice.
    ///
    /// Not all slices of [`u32`] values are valid to convert, since [`Utf32Str`] requires that it
    /// is always valid UTF-32. This function checks to ensure that the values are valid UTF-32, and
    /// then does the conversion.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_slice_unchecked_mut`][Self::from_slice_unchecked_mut], which has the same behavior
    /// but skips the check.
    ///
    /// If you need an owned string, consider using [`Utf32String::from_vec`] instead.
    ///
    /// Because you can stack-allocate a `[u32; N]`, this function is one way to have a
    /// stack-allocated string. Indeed, the [`utf32str!`][crate::utf32str] macro does exactly this
    /// after converting from UTF-8 to UTF-32.
    ///
    /// # Errors
    ///
    /// Returns an error if the slice is not UTF-32 with a description as to why the provided slice
    /// is not UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let mut sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = Utf32Str::from_slice_mut(&mut sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let mut sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    ///
    /// assert!(Utf32Str::from_slice_mut(&mut sparkle_heart).is_err());
    /// ```
    pub fn from_slice_mut(s: &mut [u32]) -> Result<&mut Self, Utf32Error> {
        validate_utf32(s)?;
        // SAFETY: Just validated
        Ok(unsafe { Self::from_slice_unchecked_mut(s) })
    }

    /// Converts a wide string slice of undefined encoding to a UTF-32 string slice without checking
    /// if the string slice is valid UTF-32.
    ///
    /// See the safe version, [`from_ustr`][Self::from_ustr], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-32. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf32Str`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf32Str, u32str};
    ///
    /// let sparkle_heart = u32str!("💖");
    /// let sparkle_heart = unsafe { Utf32Str::from_ustr_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub const unsafe fn from_ustr_unchecked(s: &crate::U32Str) -> &Self {
        Self::from_slice_unchecked(s.as_slice())
    }

    /// Converts a mutable wide string slice of undefined encoding to a mutable UTF-32 string slice
    /// without checking if the string slice is valid UTF-32.
    ///
    /// See the safe version, [`from_ustr_mut`][Self::from_ustr_mut], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-32. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf32Str`] is always valid UTF-32.
    #[inline]
    #[must_use]
    pub unsafe fn from_ustr_unchecked_mut(s: &mut crate::U32Str) -> &mut Self {
        Self::from_slice_unchecked_mut(s.as_mut_slice())
    }

    /// Converts a wide string slice of undefined encoding to a UTF-32 string slice.
    ///
    /// Since [`U32Str`] does not have a specified encoding, this conversion may fail if the
    /// [`U32Str`] does not contain valid UTF-32 data.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ustr_unchecked`][Self::from_ustr_unchecked], which has the same behavior
    /// but skips the check.
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-32 with a description as to why the
    /// provided string slice is not UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf32Str, u32str};
    ///
    /// let sparkle_heart = u32str!("💖");
    /// let sparkle_heart = Utf32Str::from_ustr(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    pub fn from_ustr(s: &crate::U32Str) -> Result<&Self, Utf32Error> {
        Self::from_slice(s.as_slice())
    }

    /// Converts a mutable wide string slice of undefined encoding to a mutable UTF-32 string slice.
    ///
    /// Since [`U32Str`] does not have a specified encoding, this conversion may fail if the
    /// [`U32Str`] does not contain valid UTF-32 data.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ustr_unchecked_mut`][Self::from_ustr_unchecked_mut], which has the same behavior
    /// but skips the check.
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-32 with a description as to why the
    /// provided string slice is not UTF-32.
    #[inline]
    pub fn from_ustr_mut(s: &mut crate::U32Str) -> Result<&mut Self, Utf32Error> {
        Self::from_slice_mut(s.as_mut_slice())
    }

    /// Converts a wide C string slice to a UTF-32 string slice without checking if the
    /// string slice is valid UTF-32.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// See the safe version, [`from_ucstr`][Self::from_ucstr], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-32. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf32Str`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf32Str, u32cstr};
    ///
    /// let sparkle_heart = u32cstr!("💖");
    /// let sparkle_heart = unsafe { Utf32Str::from_ucstr_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_ucstr_unchecked(s: &crate::U32CStr) -> &Self {
        Self::from_slice_unchecked(s.as_slice())
    }

    /// Converts a mutable wide C string slice to a mutable UTF-32 string slice without
    /// checking if the string slice is valid UTF-32.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// See the safe version, [`from_ucstr_mut`][Self::from_ucstr_mut], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string slice passed to it is
    /// valid UTF-32. If this constraint is violated, undefined behavior results as it is assumed
    /// the [`Utf32Str`] is always valid UTF-32.
    #[inline]
    #[must_use]
    pub unsafe fn from_ucstr_unchecked_mut(s: &mut crate::U32CStr) -> &mut Self {
        Self::from_slice_unchecked_mut(s.as_mut_slice())
    }

    /// Converts a wide C string slice to a UTF-32 string slice.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// Since [`U32CStr`][crate::U32CStr] does not have a specified encoding, this conversion may
    /// fail if the [`U32CStr`][crate::U32CStr] does not contain valid UTF-32 data.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ucstr_unchecked`][Self::from_ucstr_unchecked], which has the same behavior
    /// but skips the check.
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-32 with a description as to why the
    /// provided string slice is not UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{Utf32Str, u32cstr};
    ///
    /// let sparkle_heart = u32cstr!("💖");
    /// let sparkle_heart = Utf32Str::from_ucstr(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    pub fn from_ucstr(s: &crate::U32CStr) -> Result<&Self, Utf32Error> {
        Self::from_slice(s.as_slice())
    }

    /// Converts a mutable wide C string slice to a mutable UTF-32 string slice.
    ///
    /// The resulting string slice does *not* contain the nul terminator.
    ///
    /// Since [`U32CStr`][crate::U32CStr] does not have a specified encoding, this conversion may
    /// fail if the [`U32CStr`][crate::U32CStr] does not contain valid UTF-32 data.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ucstr_unchecked_mut`][Self::from_ucstr_unchecked_mut], which has the same behavior
    /// but skips the check.
    ///
    /// # Safety
    ///
    /// This method is unsafe because you can violate the invariants of [`U16CStr`][crate::U16CStr]
    /// when mutating the slice (i.e. by adding interior nul values).
    ///
    /// # Errors
    ///
    /// Returns an error if the string slice is not UTF-32 with a description as to why the
    /// provided string slice is not UTF-32.
    #[inline]
    pub unsafe fn from_ucstr_mut(s: &mut crate::U32CStr) -> Result<&mut Self, Utf32Error> {
        Self::from_slice_mut(s.as_mut_slice())
    }

    /// Converts a slice of [`char`]s to a string slice.
    ///
    /// Since [`char`] slices are always valid UTF-32, this conversion always suceeds.
    ///
    /// If you need an owned string, consider using [`Utf32String::from_chars`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let sparkle_heart = ['💖'];
    /// let sparkle_heart = Utf32Str::from_char_slice(&sparkle_heart);
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[allow(trivial_casts)]
    #[inline]
    #[must_use]
    pub const fn from_char_slice(s: &[char]) -> &Self {
        // SAFETY: char slice is always valid UTF-32
        unsafe { Self::from_slice_unchecked(&*(s as *const [char] as *const [u32])) }
    }

    /// Converts a mutable slice of [`char`]s to a string slice.
    ///
    /// Since [`char`] slices are always valid UTF-32, this conversion always suceeds.
    ///
    /// If you need an owned string, consider using [`Utf32String::from_chars`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32Str;
    ///
    /// let mut sparkle_heart = ['💖'];
    /// let sparkle_heart = Utf32Str::from_char_slice_mut(&mut sparkle_heart);
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[allow(trivial_casts)]
    #[inline]
    #[must_use]
    pub fn from_char_slice_mut(s: &mut [char]) -> &mut Self {
        // SAFETY: char slice is always valid UTF-32
        unsafe { Self::from_slice_unchecked_mut(&mut *(s as *mut [char] as *mut [u32])) }
    }

    /// Converts a string slice into a slice of [`char`]s.
    #[allow(trivial_casts)]
    #[inline]
    #[must_use]
    pub const fn as_char_slice(&self) -> &[char] {
        // SAFETY: Self should be valid UTF-32 so chars will be in range
        unsafe { &*(self.as_slice() as *const [u32] as *const [char]) }
    }

    /// Converts a mutable string slice into a mutable slice of [`char`]s.
    #[allow(trivial_casts)]
    #[inline]
    #[must_use]
    pub fn as_char_slice_mut(&mut self) -> &mut [char] {
        // SAFETY: Self should be valid UTF-32 so chars will be in range
        unsafe { &mut *(self.as_mut_slice() as *mut [u32] as *mut [char]) }
    }

    /// Converts to a standard UTF-8 [`String`].
    ///
    /// Because this string is always valid UTF-32, the conversion is lossless and non-fallible.
    #[inline]
    #[allow(clippy::inherent_to_string_shadow_display)]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_string(&self) -> String {
        let mut s = String::with_capacity(self.len());
        s.extend(self.as_char_slice());
        s
    }

    /// Returns a subslice of this string.
    ///
    /// This is the non-panicking alternative to indexing the string. Returns [`None`] whenever
    /// equivalent indexing operation would panic.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::{utf32str};
    /// let v = utf32str!("⚧️🏳️‍⚧️➡️s");
    ///
    /// assert_eq!(Some(utf32str!("⚧️")), v.get(..2));
    /// assert_eq!(Some(utf32str!("🏳️‍⚧️")), v.get(2..7));
    /// assert_eq!(Some(utf32str!("➡️")), v.get(7..9));
    /// assert_eq!(Some(utf32str!("s")), v.get(9..));
    /// ```
    #[inline]
    #[must_use]
    pub fn get<I>(&self, index: I) -> Option<&Self>
    where
        I: SliceIndex<[u32], Output = [u32]>,
    {
        // TODO: Use SliceIndex directly when it is stabilized
        // SAFETY: subslice has already been verified
        self.inner
            .get(index)
            .map(|s| unsafe { Self::from_slice_unchecked(s) })
    }

    /// Returns a mutable subslice of this string.
    ///
    /// This is the non-panicking alternative to indexing the string. Returns [`None`] whenever
    /// equivalent indexing operation would panic.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::{utf32str};
    /// # #[cfg(feature = "alloc")] {
    /// let mut v = utf32str!("⚧️🏳️‍⚧️➡️s").to_owned();
    ///
    /// assert_eq!(utf32str!("⚧️"), v.get_mut(..2).unwrap());
    /// assert_eq!(utf32str!("🏳️‍⚧️"), v.get_mut(2..7).unwrap());
    /// assert_eq!(utf32str!("➡️"), v.get_mut(7..9).unwrap());
    /// assert_eq!(utf32str!("s"), v.get_mut(9..).unwrap());
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn get_mut<I>(&mut self, index: I) -> Option<&mut Self>
    where
        I: SliceIndex<[u32], Output = [u32]>,
    {
        // TODO: Use SliceIndex directly when it is stabilized
        // SAFETY: subslice has already been verified
        self.inner
            .get_mut(index)
            .map(|s| unsafe { Self::from_slice_unchecked_mut(s) })
    }

    /// Divide one string slice into two at an index.
    ///
    /// The argument, `mid`, should be an offset from the start of the string.
    ///
    /// The two slices returned go from the start of the string slice to `mid`, and from `mid` to
    /// the end of the string slice.
    ///
    /// To get mutable string slices instead, see the [`split_at_mut`][Self::split_at_mut] method.
    ///
    /// # Panics
    ///
    /// Panics if `mid` is past the end of the last code point of the string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// let s = utf32str!("Per Martin-Löf");
    ///
    /// let (first, last) = s.split_at(3);
    ///
    /// assert_eq!("Per", first);
    /// assert_eq!(" Martin-Löf", last);
    /// ```
    #[inline]
    #[must_use]
    pub fn split_at(&self, mid: usize) -> (&Self, &Self) {
        let (a, b) = self.inner.split_at(mid);
        unsafe { (Self::from_slice_unchecked(a), Self::from_slice_unchecked(b)) }
    }

    /// Divide one mutable string slice into two at an index.
    ///
    /// The argument, `mid`, should be an offset from the start of the string.
    ///
    /// The two slices returned go from the start of the string slice to `mid`, and from `mid` to
    /// the end of the string slice.
    ///
    /// To get immutable string slices instead, see the [`split_at`][Self::split_at] method.
    ///
    /// # Panics
    ///
    /// Panics if `mid` is past the end of the last code point of the string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// # #[cfg(feature = "alloc")] {
    /// let mut s = utf32str!("Per Martin-Löf").to_owned();
    ///
    /// let (first, last) = s.split_at_mut(3);
    ///
    /// assert_eq!("Per", first);
    /// assert_eq!(" Martin-Löf", last);
    /// # }
    /// ```
    #[inline]
    #[must_use]
    pub fn split_at_mut(&mut self, mid: usize) -> (&mut Self, &mut Self) {
        let (a, b) = self.inner.split_at_mut(mid);
        unsafe {
            (
                Self::from_slice_unchecked_mut(a),
                Self::from_slice_unchecked_mut(b),
            )
        }
    }

    /// Returns an iterator over the [`char`]s of a string slice.
    ///
    /// As this string slice consists of valid UTF-32, we can iterate through a string slice by
    /// [`char`]. This method returns such an iterator.
    ///
    /// It's important to remember that [`char`] represents a Unicode Scalar Value, and might not
    /// match your idea of what a 'character' is. Iteration over grapheme clusters may be what you
    /// actually want. This functionality is not provided by this crate.
    #[inline]
    #[must_use]
    pub fn chars(&self) -> CharsUtf32<'_> {
        CharsUtf32::new(self.as_slice())
    }

    /// Returns an iterator over the [`char`]s of a string slice and their positions.
    ///
    /// As this string slice consists of valid UTF-32, we can iterate through a string slice by
    /// [`char`]. This method returns an iterator of both these [`char`]s as well as their offsets.
    ///
    /// The iterator yields tuples. The position is first, the [`char`] is second.
    #[inline]
    #[must_use]
    pub fn char_indices(&self) -> CharIndicesUtf32<'_> {
        CharIndicesUtf32::new(self.as_slice())
    }

    /// Returns an iterator of bytes over the string encoded as UTF-8.
    #[must_use]
    pub fn encode_utf8(&self) -> EncodeUtf8<CharsUtf32<'_>> {
        crate::encode_utf8(self.chars())
    }

    /// Returns an iterator of [`u16`] over the sting encoded as UTF-16.
    #[must_use]
    pub fn encode_utf16(&self) -> EncodeUtf16<CharsUtf32<'_>> {
        crate::encode_utf16(self.chars())
    }

    /// Returns an iterator that escapes each [`char`] in `self` with [`char::escape_debug`].
    #[inline]
    #[must_use]
    pub fn escape_debug(&self) -> EscapeDebug<CharsUtf32<'_>> {
        EscapeDebug::<CharsUtf32>::new(self.as_slice())
    }

    /// Returns an iterator that escapes each [`char`] in `self` with [`char::escape_default`].
    #[inline]
    #[must_use]
    pub fn escape_default(&self) -> EscapeDefault<CharsUtf32<'_>> {
        EscapeDefault::<CharsUtf32>::new(self.as_slice())
    }

    /// Returns an iterator that escapes each [`char`] in `self` with [`char::escape_unicode`].
    #[inline]
    #[must_use]
    pub fn escape_unicode(&self) -> EscapeUnicode<CharsUtf32<'_>> {
        EscapeUnicode::<CharsUtf32>::new(self.as_slice())
    }

    /// Returns the lowercase equivalent of this string slice, as a new [`Utf32String`].
    ///
    /// 'Lowercase' is defined according to the terms of the Unicode Derived Core Property
    /// `Lowercase`.
    ///
    /// Since some characters can expand into multiple characters when changing the case, this
    /// function returns a [`Utf32String`] instead of modifying the parameter in-place.
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_lowercase(&self) -> Utf32String {
        let mut s = Utf32String::with_capacity(self.len());
        for c in self.chars() {
            for lower in c.to_lowercase() {
                s.push(lower);
            }
        }
        s
    }

    /// Returns the uppercase equivalent of this string slice, as a new [`Utf32String`].
    ///
    /// 'Uppercase' is defined according to the terms of the Unicode Derived Core Property
    /// `Uppercase`.
    ///
    /// Since some characters can expand into multiple characters when changing the case, this
    /// function returns a [`Utf32String`] instead of modifying the parameter in-place.
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_uppercase(&self) -> Utf32String {
        let mut s = Utf32String::with_capacity(self.len());
        for c in self.chars() {
            for lower in c.to_uppercase() {
                s.push(lower);
            }
        }
        s
    }

    /// Returns an iterator over the lines of a [`Utf32Str`], as string slices.
    ///
    /// Lines are split at line endings that are either newlines (`\n`) or
    /// sequences of a carriage return followed by a line feed (`\r\n`).
    ///
    /// Line terminators are not included in the lines returned by the iterator.
    ///
    /// Note that any carriage return (`\r`) not immediately followed by a
    /// line feed (`\n`) does not split a line. These carriage returns are
    /// thereby included in the produced lines.
    ///
    /// The final line ending is optional. A string that ends with a final line
    /// ending will return the same lines as an otherwise identical string
    /// without a final line ending.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::utf32str;
    ///
    /// let text = utf32str!("foo\r\nbar\n\nbaz\r");
    /// let mut lines = text.lines();
    ///
    /// assert_eq!(Some(utf32str!("foo")), lines.next());
    /// assert_eq!(Some(utf32str!("bar")), lines.next());
    /// assert_eq!(Some(utf32str!("")), lines.next());
    /// // Trailing carriage return is included in the last line
    /// assert_eq!(Some(utf32str!("baz\r")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    ///
    /// The final line does not require any ending:
    ///
    /// ```
    /// use widestring::utf32str;
    ///
    /// let text = utf32str!("foo\nbar\n\r\nbaz");
    /// let mut lines = text.lines();
    ///
    /// assert_eq!(Some(utf32str!("foo")), lines.next());
    /// assert_eq!(Some(utf32str!("bar")), lines.next());
    /// assert_eq!(Some(utf32str!("")), lines.next());
    /// assert_eq!(Some(utf32str!("baz")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    pub fn lines(&self) -> Lines<'_, Utf32Str, CharIndicesUtf32<'_>> {
        Lines::new(self, self.len(), self.char_indices())
    }
}

impl AsMut<[char]> for Utf32Str {
    #[inline]
    fn as_mut(&mut self) -> &mut [char] {
        self.as_char_slice_mut()
    }
}

impl AsRef<[char]> for Utf32Str {
    #[inline]
    fn as_ref(&self) -> &[char] {
        self.as_char_slice()
    }
}

impl<'a> From<&'a [char]> for &'a Utf32Str {
    #[inline]
    fn from(value: &'a [char]) -> Self {
        Utf32Str::from_char_slice(value)
    }
}

impl<'a> From<&'a mut [char]> for &'a mut Utf32Str {
    #[inline]
    fn from(value: &'a mut [char]) -> Self {
        Utf32Str::from_char_slice_mut(value)
    }
}

impl<'a> From<&'a Utf32Str> for &'a [char] {
    #[inline]
    fn from(value: &'a Utf32Str) -> Self {
        value.as_char_slice()
    }
}

impl<'a> From<&'a mut Utf32Str> for &'a mut [char] {
    #[inline]
    fn from(value: &'a mut Utf32Str) -> Self {
        value.as_char_slice_mut()
    }
}

impl<I> Index<I> for Utf16Str
where
    I: RangeBounds<usize> + SliceIndex<[u16], Output = [u16]>,
{
    type Output = Utf16Str;

    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        self.get(index)
            .expect("index out of bounds or not on char boundary")
    }
}

impl<I> Index<I> for Utf32Str
where
    I: SliceIndex<[u32], Output = [u32]>,
{
    type Output = Utf32Str;

    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        self.get(index).expect("index out of bounds")
    }
}

impl<I> IndexMut<I> for Utf16Str
where
    I: RangeBounds<usize> + SliceIndex<[u16], Output = [u16]>,
{
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        self.get_mut(index)
            .expect("index out of bounds or not on char boundary")
    }
}

impl<I> IndexMut<I> for Utf32Str
where
    I: SliceIndex<[u32], Output = [u32]>,
{
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        self.get_mut(index).expect("index out of bounds")
    }
}

impl PartialEq<[char]> for Utf32Str {
    #[inline]
    fn eq(&self, other: &[char]) -> bool {
        self.as_char_slice() == other
    }
}

impl PartialEq<Utf32Str> for [char] {
    #[inline]
    fn eq(&self, other: &Utf32Str) -> bool {
        self == other.as_char_slice()
    }
}

impl PartialEq<Utf16Str> for Utf32Str {
    #[inline]
    fn eq(&self, other: &Utf16Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<Utf32Str> for Utf16Str {
    #[inline]
    fn eq(&self, other: &Utf32Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<&Utf16Str> for Utf32Str {
    #[inline]
    fn eq(&self, other: &&Utf16Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<&Utf32Str> for Utf16Str {
    #[inline]
    fn eq(&self, other: &&Utf32Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<Utf16Str> for &Utf32Str {
    #[inline]
    fn eq(&self, other: &Utf16Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<Utf32Str> for &Utf16Str {
    #[inline]
    fn eq(&self, other: &Utf32Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl<'a> TryFrom<&'a [u16]> for &'a Utf16Str {
    type Error = Utf16Error;

    #[inline]
    fn try_from(value: &'a [u16]) -> Result<Self, Self::Error> {
        Utf16Str::from_slice(value)
    }
}

impl<'a> TryFrom<&'a mut [u16]> for &'a mut Utf16Str {
    type Error = Utf16Error;

    #[inline]
    fn try_from(value: &'a mut [u16]) -> Result<Self, Self::Error> {
        Utf16Str::from_slice_mut(value)
    }
}

impl<'a> TryFrom<&'a [u32]> for &'a Utf32Str {
    type Error = Utf32Error;

    #[inline]
    fn try_from(value: &'a [u32]) -> Result<Self, Self::Error> {
        Utf32Str::from_slice(value)
    }
}

impl<'a> TryFrom<&'a mut [u32]> for &'a mut Utf32Str {
    type Error = Utf32Error;

    #[inline]
    fn try_from(value: &'a mut [u32]) -> Result<Self, Self::Error> {
        Utf32Str::from_slice_mut(value)
    }
}

/// Alias for [`Utf16Str`] or [`Utf32Str`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(not(windows))]
pub type WideUtfStr = Utf32Str;

/// Alias for [`Utf16Str`] or [`Utf32Str`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(windows)]
pub type WideUtfStr = Utf16Str;

#[cfg(test)]
mod test {
    use crate::*;

    #[test]
    fn utf16_trim() {
        let s = utf16str!(" Hello\tworld\t");
        assert_eq!(utf16str!("Hello\tworld\t"), s.trim_start());

        let s = utf16str!("  English  ");
        assert!(Some('E') == s.trim_start().chars().next());

        let s = utf16str!("  עברית  ");
        assert!(Some('ע') == s.trim_start().chars().next());

        let s = utf16str!("      💕Heart ");
        assert_eq!(utf16str!("💕Heart "), s.trim_start());

        let s = utf16str!(" Heart💕      ");
        assert_eq!(utf16str!(" Heart💕"), s.trim_end());
    }

    #[test]
    fn utf32_trim() {
        let s = utf32str!(" Hello\tworld\t");
        assert_eq!(utf32str!("Hello\tworld\t"), s.trim_start());

        let s = utf32str!("  English  ");
        assert!(Some('E') == s.trim_start().chars().next());

        let s = utf32str!("  עברית  ");
        assert!(Some('ע') == s.trim_start().chars().next());

        let s = utf32str!("      💕Heart ");
        assert_eq!(utf32str!("💕Heart "), s.trim_start());

        let s = utf32str!(" Heart💕      ");
        assert_eq!(utf32str!(" Heart💕"), s.trim_end());
    }
}
