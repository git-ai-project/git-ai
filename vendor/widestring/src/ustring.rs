//! Owned, growable wide strings with undefined encoding.
//!
//! This module contains wide strings and related types.

use crate::{U16CStr, U16CString, U16Str, U32CStr, U32CString, U32Str};
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
    char, cmp,
    convert::Infallible,
    fmt::Write,
    iter::FromIterator,
    mem,
    ops::{Add, AddAssign, Deref, DerefMut, Index, IndexMut, RangeBounds},
    slice::{self, SliceIndex},
    str::FromStr,
};

mod iter;

pub use iter::*;

macro_rules! ustring_common_impl {
    {
        $(#[$ustring_meta:meta])*
        struct $ustring:ident([$uchar:ty]);
        type UStr = $ustr:ident;
        type UCString = $ucstring:ident;
        type UCStr = $ucstr:ident;
        type UtfStr = $utfstr:ident;
        type UtfString = $utfstring:ident;
        $(#[$push_meta:meta])*
        fn push() -> {}
        $(#[$push_slice_meta:meta])*
        fn push_slice() -> {}
        $(#[$into_boxed_ustr_meta:meta])*
        fn into_boxed_ustr() -> {}
    } => {
        $(#[$ustring_meta])*
        #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
        #[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $ustring {
            pub(crate) inner: Vec<$uchar>,
        }

        impl $ustring {
            /// Constructs a new empty wide string.
            #[inline]
            #[must_use]
            pub const fn new() -> Self {
                Self { inner: Vec::new() }
            }

            /// Constructs a wide string from a vector.
            ///
            /// No checks are made on the contents of the vector. It may or may not be valid
            /// character data.
            ///
            /// # Examples
            ///
            /// ```rust
            /// use widestring::U16String;
            /// let v = vec![84u16, 104u16, 101u16]; // 'T' 'h' 'e'
            /// # let cloned = v.clone();
            /// // Create a wide string from the vector
            /// let wstr = U16String::from_vec(v);
            /// # assert_eq!(wstr.into_vec(), cloned);
            /// ```
            ///
            /// ```rust
            /// use widestring::U32String;
            /// let v = vec![84u32, 104u32, 101u32]; // 'T' 'h' 'e'
            /// # let cloned = v.clone();
            /// // Create a wide string from the vector
            /// let wstr = U32String::from_vec(v);
            /// # assert_eq!(wstr.into_vec(), cloned);
            /// ```
            #[inline]
            #[must_use]
            pub fn from_vec(raw: impl Into<Vec<$uchar>>) -> Self {
                Self { inner: raw.into() }
            }

            /// Constructs a wide string copy from a pointer and a length.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len` elements.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts].
            ///
            /// # Panics
            ///
            /// Panics if `len` is greater than 0 but `p` is a null pointer.
            #[must_use]
            pub unsafe fn from_ptr(p: *const $uchar, len: usize) -> Self {
                if len == 0 {
                    return Self::new();
                }
                assert!(!p.is_null());
                let slice = slice::from_raw_parts(p, len);
                Self::from_vec(slice)
            }

            /// Constructs a wide string with the given capacity.
            ///
            /// The string will be able to hold exactly `capacity` elements without reallocating.
            /// If `capacity` is set to 0, the string will not initially allocate.
            #[inline]
            #[must_use]
            pub fn with_capacity(capacity: usize) -> Self {
                Self {
                    inner: Vec::with_capacity(capacity),
                }
            }

            /// Returns the capacity this wide string can hold without reallocating.
            #[inline]
            #[must_use]
            pub fn capacity(&self) -> usize {
                self.inner.capacity()
            }

            /// Truncates the wide string to zero length.
            #[inline]
            pub fn clear(&mut self) {
                self.inner.clear()
            }

            /// Reserves the capacity for at least `additional` more capacity to be inserted in the
            /// given wide string.
            ///
            /// More space may be reserved to avoid frequent allocations.
            #[inline]
            pub fn reserve(&mut self, additional: usize) {
                self.inner.reserve(additional)
            }

            /// Reserves the minimum capacity for exactly `additional` more capacity to be inserted
            /// in the given wide string. Does nothing if the capacity is already sufficient.
            ///
            /// Note that the allocator may give more space than is requested. Therefore capacity
            /// can not be relied upon to be precisely minimal. Prefer [`reserve`][Self::reserve] if
            /// future insertions are expected.
            #[inline]
            pub fn reserve_exact(&mut self, additional: usize) {
                self.inner.reserve_exact(additional)
            }

            /// Converts the string into a [`Vec`], consuming the string in the process.
            #[inline]
            #[must_use]
            pub fn into_vec(self) -> Vec<$uchar> {
                self.inner
            }

            /// Converts to a wide string slice.
            #[inline]
            #[must_use]
            pub fn as_ustr(&self) -> &$ustr {
                $ustr::from_slice(&self.inner)
            }

            /// Converts to a mutable wide string slice.
            #[inline]
            #[must_use]
            pub fn as_mut_ustr(&mut self) -> &mut $ustr {
                $ustr::from_slice_mut(&mut self.inner)
            }

            /// Returns a [`Vec`] reference to the contents of this string.
            #[inline]
            #[must_use]
            pub fn as_vec(&self) -> &Vec<$uchar> {
                &self.inner
            }

            /// Returns a mutable reference to the contents of this string.
            #[inline]
            #[must_use]
            pub fn as_mut_vec(&mut self) -> &mut Vec<$uchar> {
                &mut self.inner
            }

            $(#[$push_meta])*
            #[inline]
            pub fn push(&mut self, s: impl AsRef<$ustr>) {
                self.inner.extend_from_slice(&s.as_ref().as_slice())
            }

            $(#[$push_slice_meta])*
            #[inline]
            pub fn push_slice(&mut self, s: impl AsRef<[$uchar]>) {
                self.inner.extend_from_slice(s.as_ref())
            }

            /// Shrinks the capacity of the wide string to match its length.
            #[inline]
            pub fn shrink_to_fit(&mut self) {
                self.inner.shrink_to_fit();
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

            $(#[$into_boxed_ustr_meta])*
            #[must_use]
            pub fn into_boxed_ustr(self) -> Box<$ustr> {
                let rw = Box::into_raw(self.inner.into_boxed_slice()) as *mut $ustr;
                unsafe { Box::from_raw(rw) }
            }

            /// Shortens this string to the specified length.
            ///
            /// If `new_len` is greater than the string's current length, this has no effect.
            ///
            /// Note that this method has no effect on the allocated capacity of the string.
            #[inline]
            pub fn truncate(&mut self, new_len: usize) {
                self.inner.truncate(new_len)
            }

            /// Inserts a string slice into this string at a specified position.
            ///
            /// This is an _O(n)_ operation as it requires copying every element in the buffer.
            ///
            /// # Panics
            ///
            /// Panics if `idx` is larger than the string's length.
            pub fn insert_ustr(&mut self, idx: usize, string: &$ustr) {
                assert!(idx <= self.len());
                self.inner
                    .resize_with(self.len() + string.len(), Default::default);
                self.inner.copy_within(idx.., idx + string.len());
                self.inner[idx..].copy_from_slice(string.as_slice());
            }

            /// Splits the string into two at the given index.
            ///
            /// Returns a newly allocated string. `self` contains values `[0, at)`, and the returned
            /// string contains values `[at, len)`.
            ///
            /// Note that the capacity of `self` does not change.
            ///
            /// # Panics
            ///
            /// Panics if `at` is equal to or greater than the length of the string.
            #[inline]
            #[must_use]
            pub fn split_off(&mut self, at: usize) -> $ustring {
                Self::from_vec(self.inner.split_off(at))
            }

            /// Retains only the elements specified by the predicate.
            ///
            /// In other words, remove all elements `e` such that `f(e)` returns `false`. This
            /// method operates in place, visiting each element exactly once in the original order,
            /// and preserves the order of the retained elements.
            pub fn retain<F>(&mut self, mut f: F)
            where
                F: FnMut($uchar) -> bool,
            {
                self.inner.retain(|e| f(*e))
            }

            /// Creates a draining iterator that removes the specified range in the string and
            /// yields the removed elements.
            ///
            /// Note: The element range is removed even if the iterator is not consumed until the
            /// end.
            ///
            /// # Panics
            ///
            /// Panics if the starting point or end point are out of bounds.
            pub fn drain<R>(&mut self, range: R) -> Drain<'_, $uchar>
            where
                R: RangeBounds<usize>,
            {
                Drain { inner: self.inner.drain(range) }
            }

            /// Removes the specified range in the string, and replaces it with the given string.
            ///
            /// The given string doesn't need to be the same length as the range.
            ///
            /// # Panics
            ///
            /// Panics if the starting point or end point are out of bounds.
            pub fn replace_range<R>(&mut self, range: R, replace_with: impl AsRef<$ustr>)
            where
                R: RangeBounds<usize>,
            {
                self.inner
                    .splice(range, replace_with.as_ref().as_slice().iter().copied());
            }
        }

        impl Add<&$ustr> for $ustring {
            type Output = $ustring;

            #[inline]
            fn add(mut self, rhs: &$ustr) -> Self::Output {
                self.push(rhs);
                self
            }
        }

        impl Add<&$ucstr> for $ustring {
            type Output = $ustring;

            #[inline]
            fn add(mut self, rhs: &$ucstr) -> Self::Output {
                self.push(rhs);
                self
            }
        }

        impl Add<&crate::$utfstr> for $ustring {
            type Output = $ustring;

            #[inline]
            fn add(mut self, rhs: &crate::$utfstr) -> Self::Output {
                self.push(rhs);
                self
            }
        }

        impl Add<&str> for $ustring {
            type Output = $ustring;

            #[inline]
            fn add(mut self, rhs: &str) -> Self::Output {
                self.push_str(rhs);
                self
            }
        }

        impl AddAssign<&$ustr> for $ustring {
            #[inline]
            fn add_assign(&mut self, rhs: &$ustr) {
                self.push(rhs)
            }
        }

        impl AddAssign<&$ucstr> for $ustring {
            #[inline]
            fn add_assign(&mut self, rhs: &$ucstr) {
                self.push(rhs)
            }
        }

        impl AddAssign<&crate::$utfstr> for $ustring {
            #[inline]
            fn add_assign(&mut self, rhs: &crate::$utfstr) {
                self.push(rhs)
            }
        }

        impl AddAssign<&str> for $ustring {
            #[inline]
            fn add_assign(&mut self, rhs: &str) {
                self.push_str(rhs);
            }
        }

        impl AsMut<$ustr> for $ustring {
            #[inline]
            fn as_mut(&mut self) -> &mut $ustr {
                self.as_mut_ustr()
            }
        }

        impl AsMut<[$uchar]> for $ustring {
            #[inline]
            fn as_mut(&mut self) -> &mut [$uchar] {
                self.as_mut_slice()
            }
        }

        impl AsRef<$ustr> for $ustring {
            #[inline]
            fn as_ref(&self) -> &$ustr {
                self.as_ustr()
            }
        }

        impl AsRef<[$uchar]> for $ustring {
            #[inline]
            fn as_ref(&self) -> &[$uchar] {
                self.as_slice()
            }
        }

        impl Borrow<$ustr> for $ustring {
            #[inline]
            fn borrow(&self) -> &$ustr {
                self.as_ustr()
            }
        }

        impl BorrowMut<$ustr> for $ustring {
            #[inline]
            fn borrow_mut(&mut self) -> &mut $ustr {
                self.as_mut_ustr()
            }
        }

        impl Default for Box<$ustr> {
            #[inline]
            fn default() -> Self {
                let boxed: Box<[$uchar]> = Box::from([]);
                let rw = Box::into_raw(boxed) as *mut $ustr;
                unsafe { Box::from_raw(rw) }
            }
        }

        impl Deref for $ustring {
            type Target = $ustr;

            #[inline]
            fn deref(&self) -> &$ustr {
                self.as_ustr()
            }
        }

        impl DerefMut for $ustring {
            #[inline]
            fn deref_mut(&mut self) -> &mut Self::Target {
                self.as_mut_ustr()
            }
        }

        impl<'a> Extend<&'a $ustr> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a $ustr>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s))
            }
        }

        impl<'a> Extend<&'a $ucstr> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a $ucstr>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s))
            }
        }

        impl<'a> Extend<&'a crate::$utfstr> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a crate::$utfstr>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s))
            }
        }

        impl<'a> Extend<&'a str> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a str>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_str(s))
            }
        }

        impl Extend<$ustring> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = $ustring>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s))
            }
        }

        impl Extend<$ucstring> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = $ucstring>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s.as_ucstr()))
            }
        }

        impl Extend<crate::$utfstring> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = crate::$utfstring>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s.as_ustr()))
            }
        }

        impl Extend<String> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = String>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push_str(s))
            }
        }

        impl Extend<char> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = char>>(&mut self, iter: T) {
                let iter = iter.into_iter();
                let (lower_bound, _) = iter.size_hint();
                self.reserve(lower_bound);
                iter.for_each(|c| self.push_char(c));
            }
        }

        impl<'a> Extend<&'a char> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = &'a char>>(&mut self, iter: T) {
                self.extend(iter.into_iter().copied())
            }
        }

        impl Extend<Box<$ustr>> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = Box<$ustr>>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s))
            }
        }

        impl<'a> Extend<Cow<'a, $ustr>> for $ustring {
            #[inline]
            fn extend<T: IntoIterator<Item = Cow<'a, $ustr>>>(&mut self, iter: T) {
                iter.into_iter().for_each(|s| self.push(s))
            }
        }

        impl From<$ustring> for Vec<$uchar> {
            #[inline]
            fn from(value: $ustring) -> Self {
                value.into_vec()
            }
        }

        impl<'a> From<$ustring> for Cow<'a, $ustr> {
            #[inline]
            fn from(s: $ustring) -> Self {
                Cow::Owned(s)
            }
        }

        impl From<Vec<$uchar>> for $ustring {
            #[inline]
            fn from(value: Vec<$uchar>) -> Self {
                Self::from_vec(value)
            }
        }

        impl From<String> for $ustring {
            #[inline]
            fn from(s: String) -> Self {
                Self::from_str(&s)
            }
        }

        impl From<&str> for $ustring {
            #[inline]
            fn from(s: &str) -> Self {
                Self::from_str(s)
            }
        }

        #[cfg(feature = "std")]
        impl From<std::ffi::OsString> for $ustring {
            #[inline]
            fn from(s: std::ffi::OsString) -> Self {
                Self::from_os_str(&s)
            }
        }

        #[cfg(feature = "std")]
        impl From<$ustring> for std::ffi::OsString {
            #[inline]
            fn from(s: $ustring) -> Self {
                s.to_os_string()
            }
        }

        impl<'a, T: ?Sized + AsRef<$ustr>> From<&'a T> for $ustring {
            #[inline]
            fn from(s: &'a T) -> Self {
                s.as_ref().to_ustring()
            }
        }

        impl<'a> From<&'a $ustr> for Cow<'a, $ustr> {
            #[inline]
            fn from(s: &'a $ustr) -> Self {
                Cow::Borrowed(s)
            }
        }

        impl<'a> From<&'a $ustr> for Box<$ustr> {
            fn from(s: &'a $ustr) -> Self {
                let boxed: Box<[$uchar]> = Box::from(&s.inner);
                let rw = Box::into_raw(boxed) as *mut $ustr;
                unsafe { Box::from_raw(rw) }
            }
        }

        impl From<Box<$ustr>> for $ustring {
            #[inline]
            fn from(boxed: Box<$ustr>) -> Self {
                boxed.into_ustring()
            }
        }

        impl From<$ustring> for Box<$ustr> {
            #[inline]
            fn from(s: $ustring) -> Self {
                s.into_boxed_ustr()
            }
        }

        impl<'a> FromIterator<&'a $ustr> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a $ustr>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl<'a> FromIterator<&'a $ucstr> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a $ucstr>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl<'a> FromIterator<&'a crate::$utfstr> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a crate::$utfstr>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl<'a> FromIterator<&'a str> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a str>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromIterator<$ustring> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = $ustring>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromIterator<$ucstring> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = $ucstring>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromIterator<crate::$utfstring> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = crate::$utfstring>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromIterator<String> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromIterator<char> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl<'a> FromIterator<&'a char> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = &'a char>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromIterator<Box<$ustr>> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = Box<$ustr>>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl<'a> FromIterator<Cow<'a, $ustr>> for $ustring {
            #[inline]
            fn from_iter<T: IntoIterator<Item = Cow<'a, $ustr>>>(iter: T) -> Self {
                let mut string = Self::new();
                string.extend(iter);
                string
            }
        }

        impl FromStr for $ustring {
            type Err = Infallible;

            #[inline]
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self::from_str(s))
            }
        }

        impl<I> Index<I> for $ustring
        where
            I: SliceIndex<[$uchar], Output = [$uchar]>,
        {
            type Output = $ustr;

            #[inline]
            fn index(&self, index: I) -> &$ustr {
                &self.as_ustr()[index]
            }
        }

        impl<I> IndexMut<I> for $ustring
        where
            I: SliceIndex<[$uchar], Output = [$uchar]>,
        {
            fn index_mut(&mut self, index: I) -> &mut Self::Output {
                &mut self.as_mut_ustr()[index]
            }
        }

        impl PartialEq<$ustr> for $ustring {
            #[inline]
            fn eq(&self, other: &$ustr) -> bool {
                self.as_ustr() == other
            }
        }

        impl PartialEq<$ucstr> for $ustring {
            #[inline]
            fn eq(&self, other: &$ucstr) -> bool {
                self.as_ustr() == other
            }
        }

        impl PartialEq<$ucstring> for $ustring {
            #[inline]
            fn eq(&self, other: &$ucstring) -> bool {
                self.as_ustr() == other.as_ucstr()
            }
        }

        impl<'a> PartialEq<&'a $ustr> for $ustring {
            #[inline]
            fn eq(&self, other: &&'a $ustr) -> bool {
                self.as_ustr() == *other
            }
        }

        impl<'a> PartialEq<&'a $ucstr> for $ustring {
            #[inline]
            fn eq(&self, other: &&'a $ucstr) -> bool {
                self.as_ustr() == *other
            }
        }

        impl<'a> PartialEq<Cow<'a, $ustr>> for $ustring {
            #[inline]
            fn eq(&self, other: &Cow<'a, $ustr>) -> bool {
                self.as_ustr() == other.as_ref()
            }
        }

        impl<'a> PartialEq<Cow<'a, $ucstr>> for $ustring {
            #[inline]
            fn eq(&self, other: &Cow<'a, $ucstr>) -> bool {
                self.as_ustr() == other.as_ref()
            }
        }

        impl PartialEq<$ustring> for $ustr {
            #[inline]
            fn eq(&self, other: &$ustring) -> bool {
                self == other.as_ustr()
            }
        }

        impl PartialEq<$ustring> for $ucstr {
            #[inline]
            fn eq(&self, other: &$ustring) -> bool {
                self.as_ustr() == other.as_ustr()
            }
        }

        impl PartialEq<$ustring> for &$ustr {
            #[inline]
            fn eq(&self, other: &$ustring) -> bool {
                self == other.as_ustr()
            }
        }

        impl PartialEq<$ustring> for &$ucstr {
            #[inline]
            fn eq(&self, other: &$ustring) -> bool {
                self.as_ustr() == other.as_ustr()
            }
        }

        impl PartialOrd<$ustr> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &$ustr) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(other)
            }
        }

        impl PartialOrd<$ucstr> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &$ucstr) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(other)
            }
        }

        impl<'a> PartialOrd<&'a $ustr> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &&'a $ustr) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(*other)
            }
        }

        impl<'a> PartialOrd<&'a $ucstr> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &&'a $ucstr) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(*other)
            }
        }

        impl<'a> PartialOrd<Cow<'a, $ustr>> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &Cow<'a, $ustr>) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(other.as_ref())
            }
        }

        impl<'a> PartialOrd<Cow<'a, $ucstr>> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &Cow<'a, $ucstr>) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(other.as_ref())
            }
        }

        impl PartialOrd<$ucstring> for $ustring {
            #[inline]
            fn partial_cmp(&self, other: &$ucstring) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(other.as_ucstr())
            }
        }

        impl ToOwned for $ustr {
            type Owned = $ustring;

            #[inline]
            fn to_owned(&self) -> $ustring {
                self.to_ustring()
            }
        }

        impl Write for $ustring {
            #[inline]
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                self.push_str(s);
                Ok(())
            }

            #[inline]
            fn write_char(&mut self, c: char) -> core::fmt::Result {
                self.push_char(c);
                Ok(())
            }
        }
    };
}

ustring_common_impl! {
    /// An owned, mutable 16-bit wide string with undefined encoding.
    ///
    /// The string slice of a [`U16String`] is [`U16Str`].
    ///
    /// [`U16String`] are strings that do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-16 data, they may be used for
    /// any wide encoded string. This is because [`U16String`] is intended to be used with FFI
    /// functions, where proper encoding cannot be guaranteed. If you need string slices that are
    /// always valid UTF-16 strings, use [`Utf16String`][crate::Utf16String] instead.
    ///
    /// Because [`U16String`] does not have a defined encoding, no restrictions are placed on
    /// mutating or indexing the string. This means that even if the string contained properly
    /// encoded UTF-16 or other encoding data, mutationing or indexing may result in malformed data.
    /// Convert to a [`Utf16String`][crate::Utf16String] if retaining proper UTF-16 encoding is
    /// desired.
    ///
    /// # FFI considerations
    ///
    /// [`U16String`] is not aware of nul values. Strings may or may not be nul-terminated, and may
    /// contain invalid and ill-formed UTF-16. These strings are intended to be used with FFI functions
    /// that directly use string length, where the strings are known to have proper nul-termination
    /// already, or where strings are merely being passed through without modification.
    ///
    /// [`U16CString`][U16CString] should be used instead if nul-aware strings are required.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U16String`] outside of FFI is with the [`u16str!`][crate::u16str]
    /// macro to convert string literals into UTF-16 string slices at compile time:
    ///
    /// ```
    /// use widestring::{u16str, U16String};
    /// let hello = U16String::from(u16str!("Hello, world!"));
    /// ```
    ///
    /// You can also convert any [`u16`] slice or vector directly:
    ///
    /// ```
    /// use widestring::{u16str, U16String};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96];
    /// let sparkle_heart = U16String::from_vec(sparkle_heart);
    ///
    /// assert_eq!(u16str!("💖"), sparkle_heart);
    ///
    /// // This unpaired UTf-16 surrogate is invalid UTF-16, but is perfectly valid in U16String
    /// let malformed_utf16 = vec![0x0, 0xd83d]; // Note that nul values are also valid an untouched
    /// let s = U16String::from_vec(malformed_utf16);
    ///
    /// assert_eq!(s.len(), 2);
    /// ```
    ///
    /// The following example constructs a [`U16String`] and shows how to convert a [`U16String`] to
    /// a regular Rust [`String`].
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "Test";
    /// // Create a wide string from the rust string
    /// let wstr = U16String::from_str(s);
    /// // Convert back to a rust string
    /// let rust_str = wstr.to_string_lossy();
    /// assert_eq!(rust_str, "Test");
    /// ```
    struct U16String([u16]);

    type UStr = U16Str;
    type UCString = U16CString;
    type UCStr = U16CStr;
    type UtfStr = Utf16Str;
    type UtfString = Utf16String;

    /// Extends the string with the given string slice.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside
    /// the string, or invalid encoding, and it is up to the caller to determine if that is
    /// acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// let mut wstr = U16String::from_str(s);
    /// let cloned = wstr.clone();
    /// // Push the clone to the end, repeating the string twice.
    /// wstr.push(cloned);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    fn push() -> {}

    /// Extends the string with the given slice.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside
    /// the string, or invalid encoding, and it is up to the caller to determine if that is
    /// acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// let mut wstr = U16String::from_str(s);
    /// let cloned = wstr.clone();
    /// // Push the clone to the end, repeating the string twice.
    /// wstr.push_slice(cloned);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    fn push_slice() -> {}

    /// Converts this wide string into a boxed string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U16String, U16Str};
    ///
    /// let s = U16String::from_str("hello");
    ///
    /// let b: Box<U16Str> = s.into_boxed_ustr();
    /// ```
    fn into_boxed_ustr() -> {}
}

ustring_common_impl! {
    /// An owned, mutable 32-bit wide string with undefined encoding.
    ///
    /// The string slice of a [`U32String`] is [`U32Str`].
    ///
    /// [`U32String`] are strings that do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-32 data, they may be used for
    /// any wide encoded string. This is because [`U32String`] is intended to be used with FFI
    /// functions, where proper encoding cannot be guaranteed. If you need string slices that are
    /// always valid UTF-32 strings, use [`Utf32String`][crate::Utf32String] instead.
    ///
    /// Because [`U32String`] does not have a defined encoding, no restrictions are placed on
    /// mutating or indexing the string. This means that even if the string contained properly
    /// encoded UTF-32 or other encoding data, mutationing or indexing may result in malformed data.
    /// Convert to a [`Utf32String`][crate::Utf32String] if retaining proper UTF-16 encoding is
    /// desired.
    ///
    /// # FFI considerations
    ///
    /// [`U32String`] is not aware of nul values. Strings may or may not be nul-terminated, and may
    /// contain invalid and ill-formed UTF-32. These strings are intended to be used with FFI functions
    /// that directly use string length, where the strings are known to have proper nul-termination
    /// already, or where strings are merely being passed through without modification.
    ///
    /// [`U32CString`][U32CString] should be used instead if nul-aware strings are required.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U32String`] outside of FFI is with the [`u32str!`][crate::u32str]
    /// macro to convert string literals into UTF-32 string slices at compile time:
    ///
    /// ```
    /// use widestring::{u32str, U32String};
    /// let hello = U32String::from(u32str!("Hello, world!"));
    /// ```
    ///
    /// You can also convert any [`u32`] slice or vector directly:
    ///
    /// ```
    /// use widestring::{u32str, U32String};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32String::from_vec(sparkle_heart);
    ///
    /// assert_eq!(u32str!("💖"), sparkle_heart);
    ///
    /// // This UTf-16 surrogate is invalid UTF-32, but is perfectly valid in U32String
    /// let malformed_utf32 = vec![0x0, 0xd83d]; // Note that nul values are also valid an untouched
    /// let s = U32String::from_vec(malformed_utf32);
    ///
    /// assert_eq!(s.len(), 2);
    /// ```
    ///
    /// The following example constructs a [`U32String`] and shows how to convert a [`U32String`] to
    /// a regular Rust [`String`].
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "Test";
    /// // Create a wide string from the rust string
    /// let wstr = U32String::from_str(s);
    /// // Convert back to a rust string
    /// let rust_str = wstr.to_string_lossy();
    /// assert_eq!(rust_str, "Test");
    /// ```
    struct U32String([u32]);

    type UStr = U32Str;
    type UCString = U32CString;
    type UCStr = U32CStr;
    type UtfStr = Utf32Str;
    type UtfString = Utf32String;

    /// Extends the string with the given string slice.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside
    /// the string, or invalid encoding, and it is up to the caller to determine if that is
    /// acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// let mut wstr = U32String::from_str(s);
    /// let cloned = wstr.clone();
    /// // Push the clone to the end, repeating the string twice.
    /// wstr.push(cloned);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    fn push() -> {}

    /// Extends the string with the given slice.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside
    /// the string, or invalid encoding, and it is up to the caller to determine if that is
    /// acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// let mut wstr = U32String::from_str(s);
    /// let cloned = wstr.clone();
    /// // Push the clone to the end, repeating the string twice.
    /// wstr.push_slice(cloned);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    fn push_slice() -> {}

    /// Converts this wide string into a boxed string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32String, U32Str};
    ///
    /// let s = U32String::from_str("hello");
    ///
    /// let b: Box<U32Str> = s.into_boxed_ustr();
    /// ```
    fn into_boxed_ustr() -> {}
}

impl U16String {
    /// Constructs a [`U16String`] copy from a [`str`], encoding it as UTF-16.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U16String`] will also be valid UTF-16.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16String::from_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), s);
    /// ```
    #[allow(clippy::should_implement_trait)]
    #[inline]
    #[must_use]
    pub fn from_str<S: AsRef<str> + ?Sized>(s: &S) -> Self {
        Self {
            inner: s.as_ref().encode_utf16().collect(),
        }
    }

    /// Constructs a [`U16String`] copy from an [`OsStr`][std::ffi::OsStr].
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U16String`] will be valid UTF-16.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms (such as
    /// windows) no changes to the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16String::from_os_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), s);
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    #[must_use]
    pub fn from_os_str<S: AsRef<std::ffi::OsStr> + ?Sized>(s: &S) -> Self {
        Self {
            inner: crate::platform::os_to_wide(s.as_ref()),
        }
    }

    /// Extends the string with the given string slice, encoding it at UTF-16.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside the
    /// string, and it is up to the caller to determine if that is acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// let mut wstr = U16String::from_str(s);
    /// // Push the original to the end, repeating the string twice.
    /// wstr.push_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    #[inline]
    pub fn push_str(&mut self, s: impl AsRef<str>) {
        self.inner.extend(s.as_ref().encode_utf16())
    }

    /// Extends the string with the given string slice.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside the
    /// string, and it is up to the caller to determine if that is acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// let mut wstr = U16String::from_str(s);
    /// // Push the original to the end, repeating the string twice.
    /// wstr.push_os_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    pub fn push_os_str(&mut self, s: impl AsRef<std::ffi::OsStr>) {
        self.inner.extend(crate::platform::os_to_wide(s.as_ref()))
    }

    /// Appends the given [`char`][prim@char] encoded as UTF-16 to the end of this string.
    #[inline]
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0; 2];
        self.inner.extend_from_slice(c.encode_utf16(&mut buf))
    }

    /// Removes the last character or unpaired surrogate from the string buffer and returns it.
    ///
    /// This method assumes UTF-16 encoding, but handles invalid UTF-16 by returning unpaired
    /// surrogates.
    ///
    /// Returns `None` if this String is empty. Otherwise, returns the character cast to a
    /// [`u32`][prim@u32] or the value of the unpaired surrogate as a [`u32`][prim@u32] value.
    pub fn pop_char(&mut self) -> Option<u32> {
        match self.inner.pop() {
            Some(low) if crate::is_utf16_surrogate(low) => {
                if !crate::is_utf16_low_surrogate(low) || self.inner.is_empty() {
                    Some(low as u32)
                } else {
                    let high = self.inner[self.len() - 1];
                    if crate::is_utf16_high_surrogate(high) {
                        self.inner.pop();
                        let buf = [high, low];
                        Some(
                            char::decode_utf16(buf.iter().copied())
                                .next()
                                .unwrap()
                                .unwrap() as u32,
                        )
                    } else {
                        Some(low as u32)
                    }
                }
            }
            Some(u) => Some(u as u32),
            None => None,
        }
    }

    /// Removes a [`char`][prim@char] or unpaired surrogate from this string at a position and
    /// returns it as a [`u32`][prim@u32].
    ///
    /// This method assumes UTF-16 encoding, but handles invalid UTF-16 by returning unpaired
    /// surrogates.
    ///
    /// This is an _O(n)_ operation, as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than or equal to the string's length.
    pub fn remove_char(&mut self, idx: usize) -> u32 {
        let slice = &self.inner[idx..];
        let c = char::decode_utf16(slice.iter().copied()).next().unwrap();
        let clen = c.as_ref().map(|c| c.len_utf16()).unwrap_or(1);
        let c = c
            .map(|c| c as u32)
            .unwrap_or_else(|_| self.inner[idx] as u32);
        self.inner.drain(idx..idx + clen);
        c
    }

    /// Inserts a character encoded as UTF-16 into this string at a specified position.
    ///
    /// This is an _O(n)_ operation as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than the string's length.
    pub fn insert_char(&mut self, idx: usize, c: char) {
        assert!(idx <= self.len());
        let mut buf = [0; 2];
        let slice = c.encode_utf16(&mut buf);
        let old_size = self.len();
        self.inner.resize(self.len() + slice.len(), 0);
        self.inner.copy_within(idx..old_size, idx + slice.len());
        self.inner[idx..idx + slice.len()].copy_from_slice(slice);
    }
}

impl U32String {
    /// Constructs a [`U32String`] from a [`char`][prim@char] vector.
    ///
    /// No checks are made on the contents of the vector.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let v: Vec<char> = "Test".chars().collect();
    /// # let cloned: Vec<u32> = v.iter().map(|&c| c as u32).collect();
    /// // Create a wide string from the vector
    /// let wstr = U32String::from_chars(v);
    /// # assert_eq!(wstr.into_vec(), cloned);
    /// ```
    #[must_use]
    pub fn from_chars(raw: impl Into<Vec<char>>) -> Self {
        let mut chars = raw.into();
        Self {
            inner: unsafe {
                let ptr = chars.as_mut_ptr() as *mut u32;
                let len = chars.len();
                let cap = chars.capacity();
                mem::forget(chars);
                Vec::from_raw_parts(ptr, len, cap)
            },
        }
    }

    /// Constructs a [`U16String`] copy from a [`str`], encoding it as UTF-32.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U32String`] will also be valid UTF-32.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32String::from_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), s);
    /// ```
    #[allow(clippy::should_implement_trait)]
    #[inline]
    #[must_use]
    pub fn from_str<S: AsRef<str> + ?Sized>(s: &S) -> Self {
        let v: Vec<char> = s.as_ref().chars().collect();
        Self::from_chars(v)
    }

    /// Constructs a [`U32String`] copy from an [`OsStr`][std::ffi::OsStr].
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U32String`] will be valid UTF-32.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms no changes to
    /// the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32String::from_os_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), s);
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[must_use]
    pub fn from_os_str<S: AsRef<std::ffi::OsStr> + ?Sized>(s: &S) -> Self {
        let v: Vec<char> = s.as_ref().to_string_lossy().chars().collect();
        Self::from_chars(v)
    }

    /// Constructs a [`U32String`] from a [`char`][prim@char] pointer and a length.
    ///
    /// The `len` argument is the number of `char` elements, **not** the number of bytes.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len`
    /// elements.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts].
    ///
    /// # Panics
    ///
    /// Panics if `len` is greater than 0 but `p` is a null pointer.
    #[inline]
    #[must_use]
    pub unsafe fn from_char_ptr(p: *const char, len: usize) -> Self {
        Self::from_ptr(p as *const u32, len)
    }

    /// Extends the string with the given string slice, encoding it at UTF-32.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside the
    /// string, and it is up to the caller to determine if that is acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// let mut wstr = U32String::from_str(s);
    /// // Push the original to the end, repeating the string twice.
    /// wstr.push_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    #[inline]
    pub fn push_str(&mut self, s: impl AsRef<str>) {
        self.inner.extend(s.as_ref().chars().map(|c| c as u32))
    }

    /// Extends the string with the given string slice.
    ///
    /// No checks are performed on the strings. It is possible to end up nul values inside the
    /// string, and it is up to the caller to determine if that is acceptable.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// let mut wstr = U32String::from_str(s);
    /// // Push the original to the end, repeating the string twice.
    /// wstr.push_os_str(s);
    ///
    /// assert_eq!(wstr.to_string().unwrap(), "MyStringMyString");
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    pub fn push_os_str(&mut self, s: impl AsRef<std::ffi::OsStr>) {
        self.inner
            .extend(s.as_ref().to_string_lossy().chars().map(|c| c as u32))
    }

    /// Appends the given [`char`][prim@char] encoded as UTF-32 to the end of this string.
    #[inline]
    pub fn push_char(&mut self, c: char) {
        self.inner.push(c as u32);
    }

    /// Removes the last value from the string buffer and returns it.
    ///
    /// This method assumes UTF-32 encoding.
    ///
    /// Returns `None` if this String is empty.
    #[inline]
    pub fn pop_char(&mut self) -> Option<u32> {
        self.inner.pop()
    }

    /// Removes a value from this string at a position and returns it.
    ///
    /// This method assumes UTF-32 encoding.
    ///
    /// This is an _O(n)_ operation, as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than or equal to the string's length.
    #[inline]
    pub fn remove_char(&mut self, idx: usize) -> u32 {
        self.inner.remove(idx)
    }

    /// Inserts a character encoded as UTF-32 into this string at a specified position.
    ///
    /// This is an _O(n)_ operation as it requires copying every element in the buffer.
    ///
    /// # Panics
    ///
    /// Panics if `idx` is larger than the string's length.
    #[inline]
    pub fn insert_char(&mut self, idx: usize, c: char) {
        self.inner.insert(idx, c as u32)
    }
}

impl core::fmt::Debug for U16String {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u16(self.as_slice(), f)
    }
}

impl core::fmt::Debug for U32String {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u32(self.as_slice(), f)
    }
}

impl From<Vec<char>> for U32String {
    #[inline]
    fn from(value: Vec<char>) -> Self {
        Self::from_chars(value)
    }
}

impl From<&[char]> for U32String {
    #[inline]
    fn from(value: &[char]) -> Self {
        U32String::from_chars(value)
    }
}

/// Alias for [`U16String`] or [`U32String`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(not(windows))]
pub type WideString = U32String;

/// Alias for [`U16String`] or [`U32String`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(windows)]
pub type WideString = U16String;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    #[allow(clippy::write_literal)]
    fn number_to_string() {
        let mut s = U16String::new();
        write!(s, "{}", 1234).unwrap();
        assert_eq!(s, U16String::from_str("1234"));
    }

    #[test]
    fn truncated_with_surrogate() {
        // Character U+24B62, encoded as D852 DF62 in UTF16
        let buf = "𤭢";
        let mut s = U16String::from_str(buf);
        assert_eq!(s.pop_char(), Some('𤭢' as u32));
    }

    #[test]
    fn insert_char() {
        // Bug #46
        let mut s = U16String::from_str("Hllo, world!");
        s.insert_char(1, 'e');
        assert_eq!(s.to_string_lossy(), "Hello, world!");
    }
}
