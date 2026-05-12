//! Owned, growable UTF strings.
//!
//! This module contains UTF strings and related types.

use crate::{
    decode_utf16_surrogate_pair,
    error::{Utf16Error, Utf32Error},
    is_utf16_low_surrogate, is_utf16_surrogate, validate_utf16, validate_utf16_vec, validate_utf32,
    validate_utf32_vec, Utf16Str, Utf32Str,
};
#[allow(unused_imports)]
use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    string::String,
    vec::Vec,
};
#[allow(unused_imports)]
use core::{
    borrow::{Borrow, BorrowMut},
    convert::{AsMut, AsRef, From, Infallible, TryFrom},
    fmt::Write,
    iter::FromIterator,
    mem,
    ops::{Add, AddAssign, Deref, DerefMut, Index, IndexMut, RangeBounds},
    ptr,
    slice::SliceIndex,
    str::FromStr,
};

mod iter;
pub use iter::*;

macro_rules! utfstring_common_impl {
    {
        $(#[$utfstring_meta:meta])*
        struct $utfstring:ident([$uchar:ty]);
        type UtfStr = $utfstr:ident;
        type UStr = $ustr:ident;
        type UCStr = $ucstr:ident;
        type UString = $ustring:ident;
        type UCString = $ucstring:ident;
        type UtfError = $utferror:ident;
        $(#[$from_vec_unchecked_meta:meta])*
        fn from_vec_unchecked() -> {}
        $(#[$from_str_meta:meta])*
        fn from_str() -> {}
        $(#[$push_utfstr_meta:meta])*
        fn push_utfstr() -> {}
        $(#[$as_mut_vec_meta:meta])*
        fn as_mut_vec() -> {}
    } => {
        $(#[$utfstring_meta])*
        #[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
        pub struct $utfstring {
            inner: Vec<$uchar>,
        }

        impl $utfstring {
            /// Creates a new empty string.
            ///
            /// Given that the string is empty, this will not allocate any initial buffer. While
            /// that means this initial operation is very inexpensive, it may cause excessive
            /// allocations later when you add data. If you have an idea of how much data the
            /// string will hold, consider [`with_capacity`][Self::with_capacity] instead to
            /// prevent excessive re-allocation.
            #[inline]
            #[must_use]
            pub const fn new() -> Self {
                Self { inner: Vec::new() }
            }

            /// Creates a new empty string with a particular capacity.
            ///
            /// This string has an internal buffer to hold its data. The capacity is the length of
            /// that buffer, and can be queried with the [`capacity`][Self::capacity] method. This
            /// method creates and empty string, but one with an initial buffer that can hold
            /// `capacity` elements. This is useful when you may be appending a bunch of data to
            /// the string, reducing the number of reallocations it needs to do.
            ///
            /// If the given capacity is `0`, no allocation will occur, and this method is identical
            /// to the [`new`][Self::new] method.
            #[inline]
            #[must_use]
            pub fn with_capacity(capacity: usize) -> Self {
                Self {
                    inner: Vec::with_capacity(capacity),
                }
            }

            $(#[$from_vec_unchecked_meta])*
            #[inline]
            #[must_use]
            pub unsafe fn from_vec_unchecked(v: impl Into<Vec<$uchar>>) -> Self {
                Self { inner: v.into() }
            }

            $(#[$from_str_meta])*
            #[inline]
            #[allow(clippy::should_implement_trait)]
            #[must_use]
            pub fn from_str<S: AsRef<str> + ?Sized>(s: &S) -> Self {
                let s = s.as_ref();
                let mut string = Self::new();
                string.extend(s.chars());
                string
            }

            /// Converts a string into a string slice.
            #[inline]
            #[must_use]
            pub fn as_utfstr(&self) -> &$utfstr {
                unsafe { $utfstr::from_slice_unchecked(self.inner.as_slice()) }
            }

            /// Converts a string into a mutable string slice.
            #[inline]
            #[must_use]
            pub fn as_mut_utfstr(&mut self) -> &mut $utfstr {
                unsafe { $utfstr::from_slice_unchecked_mut(&mut self.inner) }
            }

            /// Converts this string into a wide string of undefined encoding.
            #[inline]
            #[must_use]
            pub fn as_ustr(&self) -> &crate::$ustr {
                crate::$ustr::from_slice(self.as_slice())
            }

            /// Converts a string into a vector of its elements.
            ///
            /// This consumes the string without copying its contents.
            #[inline]
            #[must_use]
            pub fn into_vec(self) -> Vec<$uchar> {
                self.inner
            }

            $(#[$push_utfstr_meta])*
            #[inline]
            pub fn push_utfstr<S: AsRef<$utfstr> + ?Sized>(&mut self, string: &S) {
                self.inner.extend_from_slice(string.as_ref().as_slice())
            }

            /// Returns this string's capacity, in number of elements.
            #[inline]
            #[must_use]
            pub fn capacity(&self) -> usize {
                self.inner.capacity()
            }

            /// Ensures that this string's capacity is at least `additional` elements larger than
            /// its length.
            ///
            /// The capacity may be increased by more than `additional` elements if it chooses, to
            /// prevent frequent reallocations.
            ///
            /// If you do not want this "at least" behavior, see the
            /// [`reserve_exact`][Self::reserve_exact] method.
            ///
            /// # Panics
            ///
            /// Panics if the new capacity overflows [`usize`].
            #[inline]
            pub fn reserve(&mut self, additional: usize) {
                self.inner.reserve(additional)
            }

            /// Ensures that this string's capacity is `additional` elements larger than its length.
            ///
            /// Consider using the [`reserve`][Self::reserve] method unless you absolutely know
            /// better than the allocator.
            ///
            /// # Panics
            ///
            /// Panics if the new capacity overflows [`usize`].
            #[inline]
            pub fn reserve_exact(&mut self, additional: usize) {
                self.inner.reserve_exact(additional)
            }

            /// Shrinks the capacity of this string to match its length.
            #[inline]
            pub fn shrink_to_fit(&mut self) {
                self.inner.shrink_to_fit()
            }

            /// Shrinks the capacity of this string with a lower bound.
            ///
            /// The capacity will remain at least as large as both the length and the supplied
            /// value.
            ///
            /// If the current capacity is less than the lower limit, this is a no-op.
            #[inline]
            pub fn shrink_to(&mut self, min_capacity: usize) {
                self.inner.shrink_to(min_capacity)
            }

            /// Returns a slice of this string's contents.
            #[inline]
            #[must_use]
            pub fn as_slice(&self) -> &[$uchar] {
                self.inner.as_slice()
            }

            unsafe fn insert_slice(&mut self, idx: usize, slice: &[$uchar]) {
                let len = self.inner.len();
                let amt = slice.len();
                self.inner.reserve(amt);

                ptr::copy(
                    self.inner.as_ptr().add(idx),
                    self.inner.as_mut_ptr().add(idx + amt),
                    len - idx,
                );
                ptr::copy_nonoverlapping(slice.as_ptr(), self.inner.as_mut_ptr().add(idx), amt);
                self.inner.set_len(len + amt);
            }

            $(#[$as_mut_vec_meta])*
            #[inline]
            #[must_use]
            pub unsafe fn as_mut_vec(&mut self) -> &mut Vec<$uchar> {
                &mut self.inner
            }

            /// Returns the length of this string in number of elements, not [`char`]s or
            /// graphemes.
            ///
            /// In other words, it might not be what a human considers the length of the string.
            #[inline]
            #[must_use]
            pub fn len(&self) -> usize {
                self.inner.len()
            }

            /// Returns `true` if this string has a length of zero, and `false` otherwise.
            #[inline]
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.inner.is_empty()
            }

            /// Truncates the string, removing all contents.
            ///
            /// While this means the string will have a length of zero, it does not touch its
            /// capacity.
            #[inline]
            pub fn clear(&mut self) {
                self.inner.clear()
            }

            /// Converts this string into a boxed string slice.
            ///
            /// This will drop excess capacity.
            #[inline]
            #[must_use]
            pub fn into_boxed_utfstr(self) -> Box<$utfstr> {
                let slice = self.inner.into_boxed_slice();
                // SAFETY: Already valid UTF-16
                unsafe { $utfstr::from_boxed_slice_unchecked(slice) }
            }

            /// Appends a given UTF-8 string slice onto the end of this string, converting it to
            /// UTF-16.
            #[inline]
            pub fn push_str<S: AsRef<str> + ?Sized>(&mut self, string: &S) {
                self.extend(string.as_ref().chars())
            }
        }

        impl Add<&$utfstr> for $utfstring {
            type Output = $utfstring;

            #[inline]
            fn add(mut self, rhs: &$utfstr) -> Self::Output {
                self.push_utfstr(rhs);
                self
            }
        }

        impl Add<&str> for $utfstring {
            type Output = $utfstring;

            #[inline]
            fn add(mut self, rhs: &str) -> Self::Output {
                self.push_str(rhs);
                self
            }
        }

        impl AddAssign<&$utfstr> for $utfstring {
            #[inline]
            fn add_assign(&mut self, rhs: &$utfstr) {
                self.push_utfstr(rhs)
            }
        }

        impl AddAssign<&str> for $utfstring {
            #[inline]
            fn add_assign(&mut self, rhs: &str) {
                self.push_str(rhs)
            }
        }

        impl AsMut<$utfstr> for $utfstring {
            #[inline]
            fn as_mut(&mut self) -> &mut $utfstr {
                self.as_mut_utfstr()
            }
        }

        impl AsRef<$utfstr> for $utfstring {
            #[inline]
            fn as_ref(&self) -> &$utfstr {
                self.as_utfstr()
            }
        }

        impl AsRef<[$uchar]> for $utfstring {
            #[inline]
            fn as_ref(&self) -> &[$uchar] {
                &self.inner
            }
        }

        impl AsRef<crate::$ustr> for $utfstring {
            #[inline]
            fn as_ref(&self) -> &crate::$ustr {
                self.as_ustr()
            }
        }

        impl Borrow<$utfstr> for $utfstring {
            #[inline]
            fn borrow(&self) -> &$utfstr {
                self.as_utfstr()
            }
        }

        impl BorrowMut<$utfstr> for $utfstring {
            #[inline]
            fn borrow_mut(&mut self) -> &mut $utfstr {
                self.as_mut_utfstr()
            }
        }

        impl core::fmt::Debug for $utfstring {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Debug::fmt(self.as_utfstr(), f)
            }
        }

        impl Deref for $utfstring {
            type Target = $utfstr;

            #[inline]
            fn deref(&self) -> &Self::Target {
                self.as_utfstr()
            }
        }

        impl DerefMut for $utfstring {
            #[inline]
            fn deref_mut(&mut self) -> &mut Self::Target {
                self.as_mut_utfstr()
            }
        }

        impl core::fmt::Display for $utfstring {
            #[inline]
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Display::fmt(self.as_utfstr(), f)
            }
        }

        impl Extend<char> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = char>>(&mut self, iter: T) {
                let iter = iter.into_iter();
                let (lower_bound, _) = iter.size_hint();
                self.reserve(lower_bound);
                iter.for_each(|c| self.push(c));
            }
        }

        impl<'a> Extend<&'a char> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a char>>(&mut self, iter: T) {
                self.extend(iter.into_iter().copied())
            }
        }

        impl<'a> Extend<&'a $utfstr> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a $utfstr>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_utfstr(s))
            }
        }

        impl Extend<$utfstring> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = $utfstring>>(&mut self, iter: T) {
                iter.into_iter()
                    .for_each(|s| self.push_utfstr(&s))
            }
        }

        impl<'a> Extend<Cow<'a, $utfstr>> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = Cow<'a, $utfstr>>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_utfstr(&s))
            }
        }

        impl Extend<Box<$utfstr>> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = Box<$utfstr>>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_utfstr(&s))
            }
        }

        impl<'a> Extend<&'a str> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a str>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_str(s))
            }
        }

        impl Extend<String> for $utfstring {
            #[inline]
            fn extend<T: IntoIterator<Item = String>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_str(&s))
            }
        }

        impl From<&mut $utfstr> for $utfstring {
            #[inline]
            fn from(value: &mut $utfstr) -> Self {
                value.to_owned()
            }
        }

        impl From<&$utfstr> for $utfstring {
            #[inline]
            fn from(value: &$utfstr) -> Self {
                value.to_owned()
            }
        }

        impl From<&$utfstring> for $utfstring {
            #[inline]
            fn from(value: &$utfstring) -> Self {
                value.clone()
            }
        }

        impl From<$utfstring> for Cow<'_, $utfstr> {
            #[inline]
            fn from(value: $utfstring) -> Self {
                Cow::Owned(value)
            }
        }

        impl<'a> From<&'a $utfstring> for Cow<'a, $utfstr> {
            #[inline]
            fn from(value: &'a $utfstring) -> Self {
                Cow::Borrowed(value)
            }
        }

        impl From<Cow<'_, $utfstr>> for $utfstring {
            #[inline]
            fn from(value: Cow<'_, $utfstr>) -> Self {
                value.into_owned()
            }
        }

        impl From<&str> for $utfstring {
            #[inline]
            fn from(value: &str) -> Self {
                Self::from_str(value)
            }
        }

        impl From<String> for $utfstring {
            #[inline]
            fn from(value: String) -> Self {
                Self::from_str(&value)
            }
        }

        impl From<$utfstring> for crate::$ustring {
            #[inline]
            fn from(value: $utfstring) -> Self {
                crate::$ustring::from_vec(value.into_vec())
            }
        }

        impl From<&$utfstr> for String {
            #[inline]
            fn from(value: &$utfstr) -> Self {
                value.to_string()
            }
        }

        impl From<$utfstring> for String {
            #[inline]
            fn from(value: $utfstring) -> Self {
                value.to_string()
            }
        }

        #[cfg(feature = "std")]
        impl From<$utfstring> for std::ffi::OsString {
            #[inline]
            fn from(value: $utfstring) -> std::ffi::OsString {
                value.as_ustr().to_os_string()
            }
        }

        impl FromIterator<char> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl<'a> FromIterator<&'a char> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a char>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl<'a> FromIterator<&'a $utfstr> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a $utfstr>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl FromIterator<$utfstring> for $utfstring {
            fn from_iter<T: IntoIterator<Item = $utfstring>>(iter: T) -> Self {
                let mut iterator = iter.into_iter();

                // Because we're iterating over `String`s, we can avoid at least
                // one allocation by getting the first string from the iterator
                // and appending to it all the subsequent strings.
                match iterator.next() {
                    None => Self::new(),
                    Some(mut buf) => {
                        buf.extend(iterator);
                        buf
                    }
                }
            }
        }

        impl FromIterator<Box<$utfstr>> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = Box<$utfstr>>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl<'a> FromIterator<Cow<'a, $utfstr>> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = Cow<'a, $utfstr>>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl<'a> FromIterator<&'a str> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a str>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl FromIterator<String> for $utfstring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
                let mut s = Self::new();
                s.extend(iter);
                s
            }
        }

        impl FromStr for $utfstring {
            type Err = Infallible;

            #[inline]
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok($utfstring::from_str(s))
            }
        }

        impl<I> Index<I> for $utfstring
        where
            I: RangeBounds<usize> + SliceIndex<[$uchar], Output = [$uchar]>,
        {
            type Output = $utfstr;

            #[inline]
            fn index(&self, index: I) -> &Self::Output {
                &self.deref()[index]
            }
        }

        impl<I> IndexMut<I> for $utfstring
        where
            I: RangeBounds<usize> + SliceIndex<[$uchar], Output = [$uchar]>,
        {
            #[inline]
            fn index_mut(&mut self, index: I) -> &mut Self::Output {
                &mut self.deref_mut()[index]
            }
        }

        impl PartialEq<$utfstr> for $utfstring {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<&$utfstr> for $utfstring {
            #[inline]
            fn eq(&self, other: &&$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<Cow<'_, $utfstr>> for $utfstring {
            #[inline]
            fn eq(&self, other: &Cow<'_, $utfstr>) -> bool {
                self == other.as_ref()
            }
        }

        impl PartialEq<$utfstring> for Cow<'_, $utfstr> {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_ref() == other
            }
        }

        impl PartialEq<$utfstring> for $utfstr {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstring> for &$utfstr {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<str> for $utfstring {
            #[inline]
            fn eq(&self, other: &str) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<&str> for $utfstring {
            #[inline]
            fn eq(&self, other: &&str) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<$utfstring> for str {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<$utfstring> for &str {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<String> for $utfstring {
            #[inline]
            fn eq(&self, other: &String) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<$utfstring> for String {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<String> for $utfstr {
            #[inline]
            fn eq(&self, other: &String) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<$utfstr> for String {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.chars().eq(other.chars())
            }
        }

        impl PartialEq<Cow<'_, str>> for $utfstring {
            #[inline]
            fn eq(&self, other: &Cow<'_, str>) -> bool {
                self == other.as_ref()
            }
        }

        impl PartialEq<$utfstring> for Cow<'_, str> {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_ref() == other
            }
        }

        impl PartialEq<crate::$ustr> for $utfstring {
            #[inline]
            fn eq(&self, other: &crate::$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstring> for crate::$ustr {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ustring> for $utfstring {
            #[inline]
            fn eq(&self, other: &crate::$ustring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstring> for crate::$ustring {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ustring> for $utfstr {
            #[inline]
            fn eq(&self, other: &crate::$ustring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstr> for crate::$ustring {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ucstr> for $utfstring {
            #[inline]
            fn eq(&self, other: &crate::$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstring> for crate::$ucstr {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ucstring> for $utfstring {
            #[inline]
            fn eq(&self, other: &crate::$ucstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstring> for crate::$ucstring {
            #[inline]
            fn eq(&self, other: &$utfstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ucstring> for $utfstr {
            #[inline]
            fn eq(&self, other: &crate::$ucstring) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$utfstr> for crate::$ucstring {
            #[inline]
            fn eq(&self, other: &$utfstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl ToOwned for $utfstr {
            type Owned = $utfstring;

            #[inline]
            fn to_owned(&self) -> Self::Owned {
                unsafe { $utfstring::from_vec_unchecked(&self.inner) }
            }
        }

        impl TryFrom<crate::$ustring> for $utfstring {
            type Error = $utferror;

            #[inline]
            fn try_from(value: crate::$ustring) -> Result<Self, Self::Error> {
                $utfstring::from_ustring(value)
            }
        }

        impl TryFrom<crate::$ucstring> for $utfstring {
            type Error = $utferror;

            #[inline]
            fn try_from(value: crate::$ucstring) -> Result<Self, Self::Error> {
                $utfstring::from_ustring(value)
            }
        }

        impl TryFrom<&crate::$ustr> for $utfstring {
            type Error = $utferror;

            #[inline]
            fn try_from(value: &crate::$ustr) -> Result<Self, Self::Error> {
                $utfstring::from_ustring(value)
            }
        }

        impl TryFrom<&crate::$ucstr> for $utfstring {
            type Error = $utferror;

            #[inline]
            fn try_from(value: &crate::$ucstr) -> Result<Self, Self::Error> {
                $utfstring::from_ustring(value)
            }
        }

        impl Write for $utfstring {
            #[inline]
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.push_str(s);
                Ok(())
            }

            #[inline]
            fn write_char(&mut self, c: char) -> core::fmt::Result {
                self.push(c);
                Ok(())
            }
        }
    };
}

utfstring_common_impl! {
    /// A UTF-16 encoded, growable owned string.
    ///
    /// [`Utf16String`] is a version of [`String`] that uses UTF-16 encoding instead of UTF-8
    /// encoding. The equivalent of [`str`] for [`Utf16String`] is [`Utf16Str`].
    ///
    /// Unlike [`U16String`][crate::U16String] which does not specify a coding, [`Utf16String`] is
    /// always valid UTF-16 encoding. Using unsafe methods to construct a [`Utf16String`] with
    /// invalid UTF-16 encoding results in undefined behavior.
    ///
    /// # UTF-16
    ///
    /// [`Utf16String`] is always UTF-16. This means if you need non-UTF-16 wide strings, you should
    /// use [`U16String`][crate::U16String] instead. It is similar, but does not constrain the
    /// encoding.
    ///
    /// This also means you cannot directly index a single element of the string, as UTF-16 encoding
    /// may be a single `u16` value or a pair of `u16` surrogates. Instead, you can index subslices
    /// of the string, or use the [`chars`][Utf16Str::chars] iterator instead.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`Utf16String`] is with the [`utf16str!`][crate::utf16str] macro to
    /// convert string literals into UTF-16 string slices at compile time:
    ///
    /// ```
    /// use widestring::{Utf16String, utf16str};
    /// let hello = Utf16String::from(utf16str!("Hello, world!"));
    /// ```
    ///
    /// Because this string is always valid UTF-16, it is a non-fallible, lossless conversion to and
    /// from standard Rust strings:
    ///
    /// ```
    /// use widestring::Utf16String;
    /// // Unlike the utf16str macro, this will do conversion at runtime instead of compile time
    /// let hello = Utf16String::from_str("Hello, world!");
    /// let hello_string: String = hello.to_string();
    /// assert_eq!(hello, hello_string); // Can easily compare between string types
    /// ```
    struct Utf16String([u16]);

    type UtfStr = Utf16Str;
    type UStr = U16Str;
    type UCStr = U16CStr;
    type UString = U16String;
    type UCString = U16CString;
    type UtfError = Utf16Error;

    /// Converts a [`u16`] vector to a string without checking that the string contains valid
    /// UTF-16.
    ///
    /// See the safe version, [`from_vec`][Self::from_vec], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the vector passed to it is valid
    /// UTF-16. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf16String`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = unsafe { Utf16String::from_vec_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    fn from_vec_unchecked() -> {}

    /// Re-encodes a UTF-8--encoded string slice into a UTF-16--encoded string.
    ///
    /// This operation is lossless and infallible, but requires a memory allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::Utf16String;
    /// let music = Utf16String::from_str("𝄞music");
    /// assert_eq!(utf16str!("𝄞music"), music);
    /// ```
    fn from_str() -> {}

    /// Appends a given string slice onto the end of this string.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("foo");
    /// s.push_utfstr(utf16str!("bar"));
    /// assert_eq!(utf16str!("foobar"), s);
    /// ```
    fn push_utfstr() -> {}

    /// Returns a mutable reference to the contents of this string.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the values in the vector are valid
    /// UTF-16. If this constraint is violated, it may cause undefined beahvior with future
    /// users of the string, as it is assumed that this string is always valid UTF-16.
    fn as_mut_vec() -> {}
}

utfstring_common_impl! {
    /// A UTF-32 encoded, growable owned string.
    ///
    /// [`Utf32String`] is a version of [`String`] that uses UTF-32 encoding instead of UTF-8
    /// encoding. The equivalent of [`str`] for [`Utf32String`] is [`Utf32Str`].
    ///
    /// Unlike [`U32String`][crate::U32String] which does not specify a coding, [`Utf32String`] is
    /// always valid UTF-32 encoding. Using unsafe methods to construct a [`Utf32String`] with
    /// invalid UTF-32 encoding results in undefined behavior.
    ///
    /// # UTF-32
    ///
    /// [`Utf32String`] is always UTF-32. This means if you need non-UTF-32 wide strings, you should
    /// use [`U32String`][crate::U32String] instead. It is similar, but does not constrain the
    /// encoding.
    ///
    /// Unlike UTF-16 or UTF-8 strings, you may index single elements of UTF-32 strings in addition
    /// to subslicing. This is due to it being a fixed-length encoding for [`char`]s. This also
    /// means that [`Utf32String`] is the same representation as a `Vec<char>`; indeed conversions
    /// between the two exist and are simple typecasts.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`Utf32String`] is with the [`utf32str!`][crate::utf32str] macro to
    /// convert string literals into UTF-32 string slices at compile time:
    ///
    /// ```
    /// use widestring::{Utf32String, utf32str};
    /// let hello = Utf32String::from(utf32str!("Hello, world!"));
    /// ```
    ///
    /// Because this string is always valid UTF-32, it is a non-fallible, lossless conversion to and
    /// from standard Rust strings:
    ///
    /// ```
    /// use widestring::Utf32String;
    /// // Unlike the utf32str macro, this will do conversion at runtime instead of compile time
    /// let hello = Utf32String::from_str("Hello, world!");
    /// let hello_string: String = hello.to_string();
    /// assert_eq!(hello, hello_string); // Can easily compare between string types
    /// ```
    struct Utf32String([u32]);

    type UtfStr = Utf32Str;
    type UStr = U32Str;
    type UCStr = U32CStr;
    type UString = U32String;
    type UCString = U32CString;
    type UtfError = Utf32Error;

    /// Converts a [`u32`] vector to a string without checking that the string contains valid
    /// UTF-32.
    ///
    /// See the safe version, [`from_vec`][Self::from_vec], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the vector passed to it is valid
    /// UTF-32. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf32String`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = unsafe { Utf32String::from_vec_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    fn from_vec_unchecked() -> {}

    /// Re-encodes a UTF-8--encoded string slice into a UTF-32--encoded string.
    ///
    /// This operation is lossless and infallible, but requires a memory allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::Utf32String;
    /// let music = Utf32String::from_str("𝄞music");
    /// assert_eq!(utf32str!("𝄞music"), music);
    /// ```
    fn from_str() -> {}

    /// Appends a given string slice onto the end of this string.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("foo");
    /// s.push_utfstr(utf32str!("bar"));
    /// assert_eq!(utf32str!("foobar"), s);
    /// ```
    fn push_utfstr() -> {}

    /// Returns a mutable reference to the contents of this string.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the values in the vector are valid
    /// UTF-16. If this constraint is violated, it may cause undefined beahvior with future
    /// users of the string, as it is assumed that this string is always valid UTF-16.
    fn as_mut_vec() -> {}
}

impl Utf16String {
    /// Converts a [`u16`] vector of UTF-16 data to a string.
    ///
    /// Not all slices of [`u16`] values are valid to convert, since [`Utf16String`] requires that
    /// it is always valid UTF-16. This function checks to ensure that the values are valid UTF-16,
    /// and then does the conversion. This does not do any copying.
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_vec_unchecked`][Self::from_vec_unchecked], which has the same behavior but skips
    /// the check.
    ///
    /// If you need a string slice, consider using [`Utf16Str::from_slice`] instead.
    ///
    /// The inverse of this method is [`into_vec`][Self::into_vec].
    ///
    /// # Errors
    ///
    /// Returns an error if the vector is not UTF-16 with a description as to why the provided
    /// vector is not UTF-16. The error will contain the original [`Vec`] that can be reclaimed with
    /// [`into_vec`][Utf16Error::into_vec].
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = Utf16String::from_vec(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::Utf16String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    ///
    /// assert!(Utf16String::from_vec(sparkle_heart).is_err());
    /// ```
    pub fn from_vec(v: impl Into<Vec<u16>>) -> Result<Self, Utf16Error> {
        let v = validate_utf16_vec(v.into())?;
        Ok(unsafe { Self::from_vec_unchecked(v) })
    }

    /// Converts a slice of [`u16`] data to a string, including invalid characters.
    ///
    /// Since the given [`u16`] slice may not be valid UTF-16, and [`Utf16String`] requires that
    /// it is always valid UTF-16, during the conversion this function replaces any invalid UTF-16
    /// sequences with [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which
    /// looks like this: �
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the conversion, there is an unsafe version of this function,
    /// [`from_vec_unchecked`][Self::from_vec_unchecked], which has the same behavior but skips
    /// the checks.
    ///
    /// This function returns a [`Cow<'_, Utf16Str>`][std::borrow::Cow]. If the given slice is
    /// invalid UTF-16, then we need to insert our replacement characters which will change the size
    /// of the string, and hence, require an owned [`Utf16String`]. But if it's already valid
    /// UTF-16, we don't need a new allocation. This return type allows us to handle both cases.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::Utf16String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = Utf16String::from_slice_lossy(&sparkle_heart);
    ///
    /// assert_eq!(utf16str!("💖"), sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::Utf16String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    /// let sparkle_heart = Utf16String::from_slice_lossy(&sparkle_heart);
    ///
    /// assert_eq!(utf16str!("\u{fffd}\u{0000}"), sparkle_heart);
    /// ```
    #[must_use]
    pub fn from_slice_lossy(s: &[u16]) -> Cow<'_, Utf16Str> {
        match validate_utf16(s) {
            // SAFETY: validated as UTF-16
            Ok(()) => Cow::Borrowed(unsafe { Utf16Str::from_slice_unchecked(s) }),
            Err(e) => {
                let mut v = Vec::with_capacity(s.len());
                // Valid up until index
                v.extend_from_slice(&s[..e.index()]);
                let mut index = e.index();
                let mut replacement_char = [0; 2];
                let replacement_char =
                    char::REPLACEMENT_CHARACTER.encode_utf16(&mut replacement_char);
                while index < s.len() {
                    let u = s[index];
                    if is_utf16_surrogate(u) {
                        if is_utf16_low_surrogate(u) || index + 1 >= s.len() {
                            v.extend_from_slice(replacement_char);
                        } else {
                            let low = s[index + 1];
                            if is_utf16_low_surrogate(low) {
                                // Valid surrogate pair
                                v.push(u);
                                v.push(low);
                                index += 1;
                            } else {
                                v.extend_from_slice(replacement_char);
                            }
                        }
                    } else {
                        v.push(u);
                    }
                    index += 1;
                }
                // SATEFY: Is now valid UTF-16 with replacement chars
                Cow::Owned(unsafe { Self::from_vec_unchecked(v) })
            }
        }
    }

    /// Converts a wide string of undefined encoding to a UTF-16 string without checking that the
    /// string contains valid UTF-16.
    ///
    /// See the safe version, [`from_ustring`][Self::from_ustring], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string passed to it is valid
    /// UTF-16. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf16String`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U16String, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = U16String::from_vec(sparkle_heart);
    /// let sparkle_heart = unsafe { Utf16String::from_ustring_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_ustring_unchecked(s: impl Into<crate::U16String>) -> Self {
        Self::from_vec_unchecked(s.into().into_vec())
    }

    /// Converts a wide string of undefined encoding into a UTF-16 string.
    ///
    /// Not all strings with undefined encoding are valid to convert, since [`Utf16String`] requires
    /// that it is always valid UTF-16. This function checks to ensure that the string is valid
    /// UTF-16, and then does the conversion. This does not do any copying.
    ///
    /// If you are sure that the string is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ustring_unchecked`][Self::from_ustring_unchecked], which has the same behavior but
    /// skips the check.
    ///
    /// If you need a string slice, consider using [`Utf16Str::from_ustr`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not UTF-16 with a description as to why the provided
    /// string is not UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U16String, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = U16String::from_vec(sparkle_heart);
    /// let sparkle_heart = Utf16String::from_ustring(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::{U16String, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    /// let sparkle_heart = U16String::from_vec(sparkle_heart); // Valid for a U16String
    ///
    /// assert!(Utf16String::from_ustring(sparkle_heart).is_err()); // But not for a Utf16String
    /// ```
    #[inline]
    pub fn from_ustring(s: impl Into<crate::U16String>) -> Result<Self, Utf16Error> {
        Self::from_vec(s.into().into_vec())
    }

    /// Converts a wide string slice of undefined encoding of to a UTF-16 string, including invalid
    /// characters.
    ///
    /// Since the given string slice may not be valid UTF-16, and [`Utf16String`] requires that
    /// it is always valid UTF-16, during the conversion this function replaces any invalid UTF-16
    /// sequences with [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which
    /// looks like this: �
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the conversion, there is an unsafe version of this function,
    /// [`from_ustring_unchecked`][Self::from_ustring_unchecked], which has the same behavior but
    /// skips the checks.
    ///
    /// This function returns a [`Cow<'_, Utf16Str>`][std::borrow::Cow]. If the given slice is
    /// invalid UTF-16, then we need to insert our replacement characters which will change the size
    /// of the string, and hence, require an owned [`Utf16String`]. But if it's already valid
    /// UTF-16, we don't need a new allocation. This return type allows us to handle both cases.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::{U16Str, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = U16Str::from_slice(&sparkle_heart);
    /// let sparkle_heart = Utf16String::from_ustr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf16str!("💖"), sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::{U16Str, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    /// let sparkle_heart = U16Str::from_slice(&sparkle_heart);
    /// let sparkle_heart = Utf16String::from_ustr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf16str!("\u{fffd}\u{0000}"), sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_ustr_lossy(s: &crate::U16Str) -> Cow<'_, Utf16Str> {
        Self::from_slice_lossy(s.as_slice())
    }

    /// Converts a wide C string to a UTF-16 string without checking that the string contains
    /// valid UTF-16.
    ///
    /// The resulting string does *not* contain the nul terminator.
    ///
    /// See the safe version, [`from_ucstring`][Self::from_ucstring], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string passed to it is valid
    /// UTF-16. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf16String`] is always valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U16CString, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = U16CString::from_vec(sparkle_heart).unwrap();
    /// let sparkle_heart = unsafe { Utf16String::from_ucstring_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_ucstring_unchecked(s: impl Into<crate::U16CString>) -> Self {
        Self::from_vec_unchecked(s.into().into_vec())
    }

    /// Converts a wide C string into a UTF-16 string.
    ///
    /// The resulting string does *not* contain the nul terminator.
    ///
    /// Not all wide C strings are valid to convert, since [`Utf16String`] requires that
    /// it is always valid UTF-16. This function checks to ensure that the string is valid UTF-16,
    /// and then does the conversion. This does not do any copying.
    ///
    /// If you are sure that the string is valid UTF-16, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ucstring_unchecked`][Self::from_ucstring_unchecked], which has the same behavior but
    /// skips the check.
    ///
    /// If you need a string slice, consider using [`Utf16Str::from_ucstr`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not UTF-16 with a description as to why the provided
    /// string is not UTF-16.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U16CString, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // Raw surrogate pair
    /// let sparkle_heart = U16CString::from_vec(sparkle_heart).unwrap();
    /// let sparkle_heart = Utf16String::from_ucstring(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::{U16CString, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d]; // This is an invalid unpaired surrogate
    /// let sparkle_heart = U16CString::from_vec(sparkle_heart).unwrap(); // Valid for a U16CString
    ///
    /// assert!(Utf16String::from_ucstring(sparkle_heart).is_err()); // But not for a Utf16String
    /// ```
    #[inline]
    pub fn from_ucstring(s: impl Into<crate::U16CString>) -> Result<Self, Utf16Error> {
        Self::from_vec(s.into().into_vec())
    }

    /// Converts a wide C string slice of to a UTF-16 string, including invalid characters.
    ///
    /// The resulting string does *not* contain the nul terminator.
    ///
    /// Since the given string slice may not be valid UTF-16, and [`Utf16String`] requires that
    /// it is always valid UTF-16, during the conversion this function replaces any invalid UTF-16
    /// sequences with [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which
    /// looks like this: �
    ///
    /// If you are sure that the slice is valid UTF-16, and you don't want to incur the overhead of
    /// the conversion, there is an unsafe version of this function,
    /// [`from_ucstring_unchecked`][Self::from_ucstring_unchecked], which has the same behavior but
    /// skips the checks.
    ///
    /// This function returns a [`Cow<'_, Utf16Str>`][std::borrow::Cow]. If the given slice is
    /// invalid UTF-16, then we need to insert our replacement characters which will change the size
    /// of the string, and hence, require an owned [`Utf16String`]. But if it's already valid
    /// UTF-16, we don't need a new allocation. This return type allows us to handle both cases.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::{U16CStr, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96, 0x0]; // Raw surrogate pair
    /// let sparkle_heart = U16CStr::from_slice(&sparkle_heart).unwrap();
    /// let sparkle_heart = Utf16String::from_ucstr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf16str!("💖"), sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::{U16CStr, Utf16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0x0]; // This is an invalid unpaired surrogate
    /// let sparkle_heart = U16CStr::from_slice(&sparkle_heart).unwrap();
    /// let sparkle_heart = Utf16String::from_ucstr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf16str!("\u{fffd}"), sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_ucstr_lossy(s: &crate::U16CStr) -> Cow<'_, Utf16Str> {
        Self::from_slice_lossy(s.as_slice())
    }

    /// Appends the given [`char`] to the end of this string.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("abc");
    ///
    /// s.push('1');
    /// s.push('2');
    /// s.push('3');
    ///
    /// assert_eq!("abc123", s);
    /// ```
    #[inline]
    pub fn push(&mut self, ch: char) {
        let mut buf = [0; 2];
        self.inner.extend_from_slice(ch.encode_utf16(&mut buf))
    }

    /// Shortens this string to the specified length.
    ///
    /// If `new_len` is greater than the string's current length, this has no effect.
    ///
    /// Note that this method has no effect on the allocated capacity of the string.
    ///
    /// # Panics
    ///
    /// Panics if `new_len` does not lie on a [`char`] boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("hello");
    /// s.truncate(2);
    /// assert_eq!("he", s);
    /// ```
    #[inline]
    pub fn truncate(&mut self, new_len: usize) {
        if new_len <= self.len() {
            assert!(self.is_char_boundary(new_len));
            self.inner.truncate(new_len)
        }
    }

    /// Removes the last character from the string buffer and returns it.
    ///
    /// Returns [`None`] if this string is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("foo𝄞");
    ///
    /// assert_eq!(s.pop(), Some('𝄞'));
    /// assert_eq!(s.pop(), Some('o'));
    /// assert_eq!(s.pop(), Some('o'));
    /// assert_eq!(s.pop(), Some('f'));
    ///
    /// assert_eq!(s.pop(), None);
    /// ```
    pub fn pop(&mut self) -> Option<char> {
        let c = self.inner.pop();
        if let Some(c) = c {
            if is_utf16_low_surrogate(c) {
                let high = self.inner.pop().unwrap();
                // SAFETY: string is always valid UTF-16, so pair is valid
                Some(unsafe { decode_utf16_surrogate_pair(high, c) })
            } else {
                // SAFETY: not a surrogate
                Some(unsafe { char::from_u32_unchecked(c as u32) })
            }
        } else {
            None
        }
    }

    /// Removes a [`char`] from this string at an offset and returns it.
    ///
    /// This is an _O(n)_ operation, as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than or equal to the string's length, or if it does not lie on a
    /// [`char`] boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("𝄞foo");
    ///
    /// assert_eq!(s.remove(0), '𝄞');
    /// assert_eq!(s.remove(1), 'o');
    /// assert_eq!(s.remove(0), 'f');
    /// assert_eq!(s.remove(0), 'o');
    /// ```
    #[inline]
    pub fn remove(&mut self, idx: usize) -> char {
        let c = self[idx..].chars().next().unwrap();
        let next = idx + c.len_utf16();
        let len = self.len();
        unsafe {
            ptr::copy(
                self.inner.as_ptr().add(next),
                self.inner.as_mut_ptr().add(idx),
                len - next,
            );
            self.inner.set_len(len - (next - idx));
        }
        c
    }

    /// Retains only the characters specified by the predicate.
    ///
    /// In other words, remove all characters `c` such that `f(c)` returns `false`. This method
    /// operates in place, visiting each character exactly once in the original order, and preserves
    /// the order of the retained characters.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("f_o_ob_ar");
    ///
    /// s.retain(|c| c != '_');
    ///
    /// assert_eq!(s, "foobar");
    /// ```
    ///
    /// Because the elements are visited exactly once in the original order, external state may be
    /// used to decide which elements to keep.
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("abcde");
    /// let keep = [false, true, true, false, true];
    /// let mut iter = keep.iter();
    /// s.retain(|_| *iter.next().unwrap());
    /// assert_eq!(s, "bce");
    /// ```
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(char) -> bool,
    {
        let mut index = 0;
        while index < self.len() {
            // SAFETY: always in bounds and incremented by len_utf16 only
            let c = unsafe { self.get_unchecked(index..) }
                .chars()
                .next()
                .unwrap();
            if !f(c) {
                self.inner.drain(index..index + c.len_utf16());
            } else {
                index += c.len_utf16();
            }
        }
    }

    /// Inserts a character into this string at an offset.
    ///
    /// This is an _O(n)_ operation as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than the string's length, or if it does not lie on a [`char`]
    /// boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::with_capacity(5);
    ///
    /// s.insert(0, '𝄞');
    /// s.insert(0, 'f');
    /// s.insert(1, 'o');
    /// s.insert(4, 'o');
    ///
    /// assert_eq!("fo𝄞o", s);
    /// ```
    #[inline]
    pub fn insert(&mut self, idx: usize, ch: char) {
        assert!(self.is_char_boundary(idx));
        let mut bits = [0; 2];
        let bits = ch.encode_utf16(&mut bits);

        unsafe {
            self.insert_slice(idx, bits);
        }
    }

    /// Inserts a UTF-16 string slice into this string at an offset.
    ///
    /// This is an _O(n)_ operation as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than the string's length, or if it does not lie on a [`char`]
    /// boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf16str;
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("bar");
    ///
    /// s.insert_utfstr(0, utf16str!("foo"));
    ///
    /// assert_eq!("foobar", s);
    /// ```
    #[inline]
    pub fn insert_utfstr(&mut self, idx: usize, string: &Utf16Str) {
        assert!(self.is_char_boundary(idx));

        unsafe {
            self.insert_slice(idx, string.as_slice());
        }
    }

    /// Splits the string into two at the given index.
    ///
    /// Returns a newly allocated string. `self` contains elements [0, at), and the returned string
    /// contains elements [at, len). `at` must be on the boundary of a UTF-16 code point.
    ///
    /// Note that the capacity of `self` does not change.
    ///
    /// # Panics
    ///
    /// Panics if `at` is not on a UTF-16 code point boundary, or if it is beyond the last code
    /// point of the string.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut hello = Utf16String::from_str("Hello, World!");
    /// let world = hello.split_off(7);
    /// assert_eq!(hello, "Hello, ");
    /// assert_eq!(world, "World!");
    /// ```
    #[inline]
    #[must_use]
    pub fn split_off(&mut self, at: usize) -> Self {
        assert!(self.is_char_boundary(at));
        unsafe { Self::from_vec_unchecked(self.inner.split_off(at)) }
    }

    /// Creates a draining iterator that removes the specified range in the string and yields the
    /// removed [`char`]s.
    ///
    /// Note: The element range is removed even if the iterator is not consumed until the end.
    ///
    /// # Panics
    ///
    /// Panics if the starting point or end point do not lie on a [`char`] boundary, or if they're
    /// out of bounds.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::Utf16String;
    /// let mut s = Utf16String::from_str("α is alpha, β is beta");
    /// let beta_offset = 12;
    ///
    /// // Remove the range up until the β from the string
    /// let t: Utf16String = s.drain(..beta_offset).collect();
    /// assert_eq!(t, "α is alpha, ");
    /// assert_eq!(s, "β is beta");
    ///
    /// // A full range clears the string
    /// s.drain(..);
    /// assert_eq!(s, "");
    /// ```
    pub fn drain<R>(&mut self, range: R) -> DrainUtf16<'_>
    where
        R: RangeBounds<usize>,
    {
        // WARNING: Using range again would be unsound
        // TODO: replace with core::slice::range when it is stabilized
        let core::ops::Range { start, end } = crate::range(range, ..self.len());
        assert!(self.is_char_boundary(start));
        assert!(self.is_char_boundary(end));

        // Take out two simultaneous borrows. The self_ptr won't be accessed
        // until iteration is over, in Drop.
        let self_ptr: *mut _ = self;
        // SAFETY: `slice::range` and `is_char_boundary` do the appropriate bounds checks.
        let chars_iter = unsafe { self.get_unchecked(start..end) }.chars();

        DrainUtf16 {
            start,
            end,
            iter: chars_iter,
            string: self_ptr,
        }
    }

    /// Removes the specified range in the string, and replaces it with the given string.
    ///
    /// The given string doesn't need to be the same length as the range.
    ///
    /// # Panics
    ///
    /// Panics if the starting point or end point do not lie on a [`char`] boundary, or if they're
    /// out of bounds.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::{utf16str, Utf16String};
    /// let mut s = Utf16String::from_str("α is alpha, β is beta");
    /// let beta_offset = 12;
    ///
    /// // Replace the range up until the β from the string
    /// s.replace_range(..beta_offset, utf16str!("Α is capital alpha; "));
    /// assert_eq!(s, "Α is capital alpha; β is beta");
    /// ```
    pub fn replace_range<R>(&mut self, range: R, replace_with: &Utf16Str)
    where
        R: RangeBounds<usize>,
    {
        use core::ops::Bound::*;

        // WARNING: Using range again would be unsound
        let start = range.start_bound();
        match start {
            Included(&n) => assert!(self.is_char_boundary(n)),
            Excluded(&n) => assert!(self.is_char_boundary(n + 1)),
            Unbounded => {}
        };
        // WARNING: Inlining this variable would be unsound
        let end = range.end_bound();
        match end {
            Included(&n) => assert!(self.is_char_boundary(n + 1)),
            Excluded(&n) => assert!(self.is_char_boundary(n)),
            Unbounded => {}
        };

        // Using `range` again would be unsound
        // We assume the bounds reported by `range` remain the same, but
        // an adversarial implementation could change between calls
        self.inner
            .splice((start, end), replace_with.as_slice().iter().copied());
    }
}

impl Utf32String {
    /// Converts a [`u32`] vector of UTF-32 data to a string.
    ///
    /// Not all slices of [`u32`] values are valid to convert, since [`Utf32String`] requires that
    /// it is always valid UTF-32. This function checks to ensure that the values are valid UTF-32,
    /// and then does the conversion. This does not do any copying.
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_vec_unchecked`][Self::from_vec_unchecked], which has the same behavior but skips
    /// the check.
    ///
    /// If you need a string slice, consider using [`Utf32Str::from_slice`] instead.
    ///
    /// The inverse of this method is [`into_vec`][Self::into_vec].
    ///
    /// # Errors
    ///
    /// Returns an error if the vector is not UTF-32 with a description as to why the provided
    /// vector is not UTF-32. The error will contain the original [`Vec`] that can be reclaimed with
    /// [`into_vec`][Utf32Error::into_vec].
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = Utf32String::from_vec(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::Utf32String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    ///
    /// assert!(Utf32String::from_vec(sparkle_heart).is_err());
    /// ```
    pub fn from_vec(v: impl Into<Vec<u32>>) -> Result<Self, Utf32Error> {
        let v = validate_utf32_vec(v.into())?;
        Ok(unsafe { Self::from_vec_unchecked(v) })
    }

    /// Converts a slice of [`u32`] data to a string, including invalid characters.
    ///
    /// Since the given [`u32`] slice may not be valid UTF-32, and [`Utf32String`] requires that
    /// it is always valid UTF-32, during the conversion this function replaces any invalid UTF-32
    /// sequences with [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which
    /// looks like this: �
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the conversion, there is an unsafe version of this function,
    /// [`from_vec_unchecked`][Self::from_vec_unchecked], which has the same behavior but skips
    /// the checks.
    ///
    /// This function returns a [`Cow<'_, Utf32Str>`][std::borrow::Cow]. If the given slice is
    /// invalid UTF-32, then we need to insert our replacement characters which will change the size
    /// of the string, and hence, require an owned [`Utf32String`]. But if it's already valid
    /// UTF-32, we don't need a new allocation. This return type allows us to handle both cases.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::Utf32String;
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = Utf32String::from_slice_lossy(&sparkle_heart);
    ///
    /// assert_eq!(utf32str!("💖"), sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::Utf32String;
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    /// let sparkle_heart = Utf32String::from_slice_lossy(&sparkle_heart);
    ///
    /// assert_eq!(utf32str!("\u{fffd}\u{fffd}"), sparkle_heart);
    /// ```
    #[must_use]
    pub fn from_slice_lossy(s: &[u32]) -> Cow<'_, Utf32Str> {
        match validate_utf32(s) {
            // SAFETY: validated as UTF-32
            Ok(()) => Cow::Borrowed(unsafe { Utf32Str::from_slice_unchecked(s) }),
            Err(e) => {
                let mut v = Vec::with_capacity(s.len());
                // Valid up until index
                v.extend_from_slice(&s[..e.index()]);
                for u in s[e.index()..].iter().copied() {
                    if char::from_u32(u).is_some() {
                        v.push(u);
                    } else {
                        v.push(char::REPLACEMENT_CHARACTER as u32);
                    }
                }
                // SATEFY: Is now valid UTF-32 with replacement chars
                Cow::Owned(unsafe { Self::from_vec_unchecked(v) })
            }
        }
    }

    /// Converts a wide string of undefined encoding to a UTF-32 string without checking that the
    /// string contains valid UTF-32.
    ///
    /// See the safe version, [`from_ustring`][Self::from_ustring], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string passed to it is valid
    /// UTF-32. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf32String`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32String, Utf32String};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32String::from_vec(sparkle_heart);
    /// let sparkle_heart = unsafe { Utf32String::from_ustring_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_ustring_unchecked(s: impl Into<crate::U32String>) -> Self {
        Self::from_vec_unchecked(s.into().into_vec())
    }

    /// Converts a wide string of undefined encoding string into a UTF-32 string.
    ///
    /// Not all strings of undefined encoding are valid to convert, since [`Utf32String`] requires
    /// that it is always valid UTF-32. This function checks to ensure that the string is valid
    /// UTF-32, and then does the conversion. This does not do any copying.
    ///
    /// If you are sure that the string is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ustring_unchecked`][Self::from_ustring_unchecked], which has the same behavior but
    /// skips the check.
    ///
    /// If you need a string slice, consider using [`Utf32Str::from_ustr`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not UTF-32 with a description as to why the provided
    /// string is not UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32String, Utf32String};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32String::from_vec(sparkle_heart);
    /// let sparkle_heart = Utf32String::from_ustring(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::{U32String, Utf32String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    /// let sparkle_heart = U32String::from_vec(sparkle_heart); // Valid for a U32String
    ///
    /// assert!(Utf32String::from_ustring(sparkle_heart).is_err()); // But not for a Utf32String
    /// ```
    #[inline]
    pub fn from_ustring(s: impl Into<crate::U32String>) -> Result<Self, Utf32Error> {
        Self::from_vec(s.into().into_vec())
    }

    /// Converts a wide string slice of undefined encoding to a UTF-32 string, including invalid
    /// characters.
    ///
    /// Since the given string slice may not be valid UTF-32, and [`Utf32String`] requires that
    /// it is always valid UTF-32, during the conversion this function replaces any invalid UTF-32
    /// sequences with [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which
    /// looks like this: �
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the conversion, there is an unsafe version of this function,
    /// [`from_ustring_unchecked`][Self::from_ustring_unchecked], which has the same behavior but
    /// skips the checks.
    ///
    /// This function returns a [`Cow<'_, Utf32Str>`][std::borrow::Cow]. If the given slice is
    /// invalid UTF-32, then we need to insert our replacement characters which will change the size
    /// of the string, and hence, require an owned [`Utf32String`]. But if it's already valid
    /// UTF-32, we don't need a new allocation. This return type allows us to handle both cases.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::{U32Str, Utf32String};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32Str::from_slice(&sparkle_heart);
    /// let sparkle_heart = Utf32String::from_ustr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf32str!("💖"), sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::{U32Str, Utf32String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    /// let sparkle_heart = U32Str::from_slice(&sparkle_heart);
    /// let sparkle_heart = Utf32String::from_ustr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf32str!("\u{fffd}\u{fffd}"), sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_ustr_lossy(s: &crate::U32Str) -> Cow<'_, Utf32Str> {
        Self::from_slice_lossy(s.as_slice())
    }

    /// Converts a wide C string to a UTF-32 string without checking that the string contains
    /// valid UTF-32.
    ///
    /// The resulting string does *not* contain the nul terminator.
    ///
    /// See the safe version, [`from_ucstring`][Self::from_ucstring], for more information.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it does not check that the string passed to it is valid
    /// UTF-32. If this constraint is violated, undefined behavior results as it is assumed the
    /// [`Utf32String`] is always valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32CString, Utf32String};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32CString::from_vec(sparkle_heart).unwrap();
    /// let sparkle_heart = unsafe { Utf32String::from_ucstring_unchecked(sparkle_heart) };
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_ucstring_unchecked(s: impl Into<crate::U32CString>) -> Self {
        Self::from_vec_unchecked(s.into().into_vec())
    }

    /// Converts a wide C string into a UTF-32 string.
    ///
    /// The resulting string does *not* contain the nul terminator.
    ///
    /// Not all wide C strings are valid to convert, since [`Utf32String`] requires that
    /// it is always valid UTF-32. This function checks to ensure that the string is valid UTF-32,
    /// and then does the conversion. This does not do any copying.
    ///
    /// If you are sure that the string is valid UTF-32, and you don't want to incur the overhead of
    /// the validity check, there is an unsafe version of this function,
    /// [`from_ucstring_unchecked`][Self::from_ucstring_unchecked], which has the same behavior but
    /// skips the check.
    ///
    /// If you need a string slice, consider using [`Utf32Str::from_ucstr`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not UTF-32 with a description as to why the provided
    /// string is not UTF-32.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32CString, Utf32String};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32CString::from_vec(sparkle_heart).unwrap();
    /// let sparkle_heart = Utf32String::from_ucstring(sparkle_heart).unwrap();
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// use widestring::{U32CString, Utf32String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96]; // UTF-16 surrogates are invalid
    /// let sparkle_heart = U32CString::from_vec(sparkle_heart).unwrap(); // Valid for a U32CString
    ///
    /// assert!(Utf32String::from_ucstring(sparkle_heart).is_err()); // But not for a Utf32String
    /// ```
    #[inline]
    pub fn from_ucstring(s: impl Into<crate::U32CString>) -> Result<Self, Utf32Error> {
        Self::from_vec(s.into().into_vec())
    }

    /// Converts a wide C string slice of to a UTF-32 string, including invalid characters.
    ///
    /// The resulting string does *not* contain the nul terminator.
    ///
    /// Since the given string slice may not be valid UTF-32, and [`Utf32String`] requires that
    /// it is always valid UTF-32, during the conversion this function replaces any invalid UTF-32
    /// sequences with [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which
    /// looks like this: �
    ///
    /// If you are sure that the slice is valid UTF-32, and you don't want to incur the overhead of
    /// the conversion, there is an unsafe version of this function,
    /// [`from_ucstring_unchecked`][Self::from_ucstring_unchecked], which has the same behavior but
    /// skips the checks.
    ///
    /// This function returns a [`Cow<'_, Utf32Str>`][std::borrow::Cow]. If the given slice is
    /// invalid UTF-32, then we need to insert our replacement characters which will change the size
    /// of the string, and hence, require an owned [`Utf32String`]. But if it's already valid
    /// UTF-32, we don't need a new allocation. This return type allows us to handle both cases.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::{U32CStr, Utf32String};
    ///
    /// let sparkle_heart = vec![0x1f496, 0x0];
    /// let sparkle_heart = U32CStr::from_slice(&sparkle_heart).unwrap();
    /// let sparkle_heart = Utf32String::from_ucstr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf32str!("💖"), sparkle_heart);
    /// ```
    ///
    /// With incorrect values that return an error:
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::{U32CStr, Utf32String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96, 0x0]; // UTF-16 surrogates are invalid
    /// let sparkle_heart = U32CStr::from_slice(&sparkle_heart).unwrap();
    /// let sparkle_heart = Utf32String::from_ucstr_lossy(sparkle_heart);
    ///
    /// assert_eq!(utf32str!("\u{fffd}\u{fffd}"), sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_ucstr_lossy(s: &crate::U32CStr) -> Cow<'_, Utf32Str> {
        Self::from_slice_lossy(s.as_slice())
    }

    /// Converts a vector of [`char`]s into a UTF-32 string.
    ///
    /// Since [`char`]s are always valid UTF-32, this is infallible and efficient.
    ///
    /// If you need a string slice, consider using [`Utf32Str::from_char_slice`] instead.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32CString, Utf32String};
    ///
    /// let sparkle_heart = vec!['💖'];
    /// let sparkle_heart = Utf32String::from_chars(sparkle_heart);
    ///
    /// assert_eq!("💖", sparkle_heart);
    /// ```
    #[inline]
    #[must_use]
    pub fn from_chars(s: impl Into<Vec<char>>) -> Self {
        // SAFETY: Char slices are always valid UTF-32
        // TODO: replace mem:transmute when Vec::into_raw_parts is stabilized
        // Clippy reports this is unsound due to different sized types; but the sizes are the same
        // size. Still best to swap to Vec::into_raw_parts asap.
        #[allow(clippy::unsound_collection_transmute)]
        unsafe {
            let vec: Vec<u32> = mem::transmute(s.into());
            Self::from_vec_unchecked(vec)
        }
    }

    /// Appends the given [`char`] to the end of this string.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("abc");
    ///
    /// s.push('1');
    /// s.push('2');
    /// s.push('3');
    ///
    /// assert_eq!("abc123", s);
    /// ```
    #[inline]
    pub fn push(&mut self, ch: char) {
        self.inner.push(ch.into())
    }

    /// Shortens this string to the specified length.
    ///
    /// If `new_len` is greater than the string's current length, this has no effect.
    ///
    /// Note that this method has no effect on the allocated capacity of the string.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("hello");
    /// s.truncate(2);
    /// assert_eq!("he", s);
    /// ```
    #[inline]
    pub fn truncate(&mut self, new_len: usize) {
        self.inner.truncate(new_len)
    }

    /// Removes the last character from the string buffer and returns it.
    ///
    /// Returns [`None`] if this string is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("foo");
    ///
    /// assert_eq!(s.pop(), Some('o'));
    /// assert_eq!(s.pop(), Some('o'));
    /// assert_eq!(s.pop(), Some('f'));
    ///
    /// assert_eq!(s.pop(), None);
    /// ```
    #[inline]
    pub fn pop(&mut self) -> Option<char> {
        // SAFETY: String is already valid UTF-32
        self.inner
            .pop()
            .map(|c| unsafe { core::char::from_u32_unchecked(c) })
    }

    /// Removes a [`char`] from this string at an offset and returns it.
    ///
    /// This is an _O(n)_ operation, as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than or equal to the string's length.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("foo");
    ///
    /// assert_eq!(s.remove(1), 'o');
    /// assert_eq!(s.remove(0), 'f');
    /// assert_eq!(s.remove(0), 'o');
    /// ```
    #[inline]
    pub fn remove(&mut self, idx: usize) -> char {
        let next = idx + 1;
        let len = self.len();
        unsafe {
            let c = core::char::from_u32_unchecked(self.inner[idx]);
            ptr::copy(
                self.inner.as_ptr().add(next),
                self.inner.as_mut_ptr().add(idx),
                len - next,
            );
            self.inner.set_len(len - (next - idx));
            c
        }
    }

    /// Retains only the characters specified by the predicate.
    ///
    /// In other words, remove all characters `c` such that `f(c)` returns `false`. This method
    /// operates in place, visiting each character exactly once in the original order, and preserves
    /// the order of the retained characters.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("f_o_ob_ar");
    ///
    /// s.retain(|c| c != '_');
    ///
    /// assert_eq!(s, "foobar");
    /// ```
    ///
    /// Because the elements are visited exactly once in the original order, external state may be
    /// used to decide which elements to keep.
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("abcde");
    /// let keep = [false, true, true, false, true];
    /// let mut iter = keep.iter();
    /// s.retain(|_| *iter.next().unwrap());
    /// assert_eq!(s, "bce");
    /// ```
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(char) -> bool,
    {
        let mut index = 0;
        while index < self.len() {
            // SAFETY: always in bounds
            let c = unsafe { self.get_unchecked(index..) }
                .chars()
                .next()
                .unwrap();
            if !f(c) {
                self.inner.remove(index);
            } else {
                index += 1;
            }
        }
    }

    /// Inserts a character into this string at an offset.
    ///
    /// This is an _O(n)_ operation as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than the string's length.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::with_capacity(3);
    ///
    /// s.insert(0, 'f');
    /// s.insert(1, 'o');
    /// s.insert(1, 'o');
    ///
    /// assert_eq!("foo", s);
    /// ```
    #[inline]
    pub fn insert(&mut self, idx: usize, ch: char) {
        unsafe {
            self.insert_slice(idx, &[ch as u32]);
        }
    }

    /// Inserts a UTF-32 string slice into this string at an offset.
    ///
    /// This is an _O(n)_ operation as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than the string's length.
    ///
    /// # Examples
    ///
    /// ```
    /// # use widestring::utf32str;
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("bar");
    ///
    /// s.insert_utfstr(0, utf32str!("foo"));
    ///
    /// assert_eq!("foobar", s);
    /// ```
    #[inline]
    pub fn insert_utfstr(&mut self, idx: usize, string: &Utf32Str) {
        unsafe {
            self.insert_slice(idx, string.as_slice());
        }
    }

    /// Splits the string into two at the given index.
    ///
    /// Returns a newly allocated string. `self` contains elements [0, at), and the returned string
    /// contains elements [at, len).
    ///
    /// Note that the capacity of `self` does not change.
    ///
    /// # Panics
    ///
    /// Panics if `at`it is beyond the last code point of the string.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut hello = Utf32String::from_str("Hello, World!");
    /// let world = hello.split_off(7);
    /// assert_eq!(hello, "Hello, ");
    /// assert_eq!(world, "World!");
    /// ```
    #[inline]
    #[must_use]
    pub fn split_off(&mut self, at: usize) -> Self {
        unsafe { Self::from_vec_unchecked(self.inner.split_off(at)) }
    }

    /// Creates a draining iterator that removes the specified range in the string and yields the
    /// removed [`char`]s.
    ///
    /// Note: The element range is removed even if the iterator is not consumed until the end.
    ///
    /// # Panics
    ///
    /// Panics if the starting point or end point are out of bounds.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::Utf32String;
    /// let mut s = Utf32String::from_str("α is alpha, β is beta");
    /// let beta_offset = 12;
    ///
    /// // Remove the range up until the β from the string
    /// let t: Utf32String = s.drain(..beta_offset).collect();
    /// assert_eq!(t, "α is alpha, ");
    /// assert_eq!(s, "β is beta");
    ///
    /// // A full range clears the string
    /// s.drain(..);
    /// assert_eq!(s, "");
    /// ```
    pub fn drain<R>(&mut self, range: R) -> DrainUtf32<'_>
    where
        R: RangeBounds<usize>,
    {
        // WARNING: Using range again would be unsound
        // TODO: replace with core::slice::range when it is stabilized
        let core::ops::Range { start, end } = crate::range(range, ..self.len());

        // Take out two simultaneous borrows. The self_ptr won't be accessed
        // until iteration is over, in Drop.
        let self_ptr: *mut _ = self;
        // SAFETY: `slice::range` and `is_char_boundary` do the appropriate bounds checks.
        let chars_iter = unsafe { self.get_unchecked(start..end) }.chars();

        DrainUtf32 {
            start,
            end,
            iter: chars_iter,
            string: self_ptr,
        }
    }

    /// Removes the specified range in the string, and replaces it with the given string.
    ///
    /// The given string doesn't need to be the same length as the range.
    ///
    /// # Panics
    ///
    /// Panics if the starting point or end point are out of bounds.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::{utf32str, Utf32String};
    /// let mut s = Utf32String::from_str("α is alpha, β is beta");
    /// let beta_offset = 12;
    ///
    /// // Replace the range up until the β from the string
    /// s.replace_range(..beta_offset, utf32str!("Α is capital alpha; "));
    /// assert_eq!(s, "Α is capital alpha; β is beta");
    /// ```
    #[inline]
    pub fn replace_range<R>(&mut self, range: R, replace_with: &Utf32Str)
    where
        R: RangeBounds<usize>,
    {
        self.inner
            .splice(range, replace_with.as_slice().iter().copied());
    }

    /// Converts string into a [`Vec`] of [`char`]s.
    ///
    /// This consumes the string without copying its contents.
    #[allow(trivial_casts)]
    #[inline]
    #[must_use]
    pub fn into_char_vec(self) -> Vec<char> {
        let mut v = mem::ManuallyDrop::new(self.into_vec());
        let (ptr, len, cap) = (v.as_mut_ptr(), v.len(), v.capacity());
        // SAFETY: Self should be valid UTF-32 so chars will be in range
        unsafe { Vec::from_raw_parts(ptr as *mut char, len, cap) }
    }
}

impl AsMut<[char]> for Utf32String {
    #[inline]
    fn as_mut(&mut self) -> &mut [char] {
        self.as_char_slice_mut()
    }
}

impl AsRef<[char]> for Utf32String {
    #[inline]
    fn as_ref(&self) -> &[char] {
        self.as_char_slice()
    }
}

impl From<Vec<char>> for Utf32String {
    #[inline]
    fn from(value: Vec<char>) -> Self {
        Utf32String::from_chars(value)
    }
}

impl From<&[char]> for Utf32String {
    #[inline]
    fn from(value: &[char]) -> Self {
        Utf32String::from_chars(value)
    }
}

impl From<Utf32String> for Vec<char> {
    #[inline]
    fn from(value: Utf32String) -> Self {
        value.into_char_vec()
    }
}

impl PartialEq<[char]> for Utf32String {
    #[inline]
    fn eq(&self, other: &[char]) -> bool {
        self.as_char_slice() == other
    }
}

impl PartialEq<Utf16String> for Utf32String {
    #[inline]
    fn eq(&self, other: &Utf16String) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<Utf32String> for Utf16String {
    #[inline]
    fn eq(&self, other: &Utf32String) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<&Utf16Str> for Utf32String {
    #[inline]
    fn eq(&self, other: &&Utf16Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<&Utf32Str> for Utf16String {
    #[inline]
    fn eq(&self, other: &&Utf32Str) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<Utf32String> for &Utf16Str {
    #[inline]
    fn eq(&self, other: &Utf32String) -> bool {
        self.chars().eq(other.chars())
    }
}

impl PartialEq<Utf16String> for &Utf32Str {
    #[inline]
    fn eq(&self, other: &Utf16String) -> bool {
        self.chars().eq(other.chars())
    }
}

impl TryFrom<Vec<u16>> for Utf16String {
    type Error = Utf16Error;

    #[inline]
    fn try_from(value: Vec<u16>) -> Result<Self, Self::Error> {
        Utf16String::from_vec(value)
    }
}

impl TryFrom<Vec<u32>> for Utf32String {
    type Error = Utf32Error;

    #[inline]
    fn try_from(value: Vec<u32>) -> Result<Self, Self::Error> {
        Utf32String::from_vec(value)
    }
}

impl TryFrom<&[u16]> for Utf16String {
    type Error = Utf16Error;

    #[inline]
    fn try_from(value: &[u16]) -> Result<Self, Self::Error> {
        Utf16String::from_vec(value)
    }
}

impl TryFrom<&[u32]> for Utf32String {
    type Error = Utf32Error;

    #[inline]
    fn try_from(value: &[u32]) -> Result<Self, Self::Error> {
        Utf32String::from_vec(value)
    }
}

/// Alias for [`Utf16String`] or [`Utf32String`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(not(windows))]
pub type WideUtfString = Utf32String;

/// Alias for [`Utf16String`] or [`Utf32String`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(windows)]
pub type WideUtfString = Utf16String;

#[cfg(test)]
mod test {
    use crate::*;

    #[test]
    fn uft16_truncate() {
        // Bug #39
        let cs = utf16str!("trunc");
        let mut s: Utf16String = cs.into();
        s.truncate(6);
        assert_eq!(s.len(), 5);
        s.truncate(5);
        assert_eq!(s.len(), 5);
        s.truncate(2);
        assert_eq!(s.len(), 2);
    }
}
