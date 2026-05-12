//! Wide string slices with undefined encoding.
//!
//! This module contains wide string slices and related types.

use crate::utfstr::Lines;
#[cfg(feature = "alloc")]
use crate::{
    error::{Utf16Error, Utf32Error},
    U16String, U32String,
};
#[cfg(feature = "alloc")]
#[allow(unused_imports)]
use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    char,
    fmt::Write,
    ops::{Index, IndexMut, Range},
    slice::{self, SliceIndex},
};

mod iter;

pub use iter::*;

macro_rules! ustr_common_impl {
    {
        $(#[$ustr_meta:meta])*
        struct $ustr:ident([$uchar:ty]);
        type UString = $ustring:ident;
        type UCStr = $ucstr:ident;
        $(#[$display_meta:meta])*
        fn display() -> {}
    } => {
        $(#[$ustr_meta])*
        #[allow(clippy::derive_hash_xor_eq)]
        #[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $ustr {
            pub(crate) inner: [$uchar],
        }

        impl $ustr {
            /// Coerces a value into a wide string slice.
            #[inline]
            #[must_use]
            pub fn new<S: AsRef<Self> + ?Sized>(s: &S) -> &Self {
                s.as_ref()
            }

            /// Constructs a wide string slice from a pointer and a length.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes. No
            /// copying or allocation is performed, the resulting value is a direct reference to the
            /// pointer bytes.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len` elements.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts]. In particular, the returned string reference *must not
            /// be mutated* for the duration of lifetime `'a`, except inside an
            /// [`UnsafeCell`][std::cell::UnsafeCell].
            ///
            /// # Panics
            ///
            /// This function panics if `p` is null.
            ///
            /// # Caveat
            ///
            /// The lifetime for the returned string is inferred from its usage. To prevent
            /// accidental misuse, it's suggested to tie the lifetime to whichever source lifetime
            /// is safe in the context, such as by providing a helper function taking the lifetime
            /// of a host value for the string, or by explicit annotation.
            #[inline]
            #[must_use]
            pub unsafe fn from_ptr<'a>(p: *const $uchar, len: usize) -> &'a Self {
                assert!(!p.is_null());
                let slice: *const [$uchar] = slice::from_raw_parts(p, len);
                &*(slice as *const $ustr)
            }

            /// Constructs a mutable wide string slice from a mutable pointer and a length.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes. No
            /// copying or allocation is performed, the resulting value is a direct reference to the
            /// pointer bytes.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len` elements.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts_mut].
            ///
            /// # Panics
            ///
            /// This function panics if `p` is null.
            ///
            /// # Caveat
            ///
            /// The lifetime for the returned string is inferred from its usage. To prevent
            /// accidental misuse, it's suggested to tie the lifetime to whichever source lifetime
            /// is safe in the context, such as by providing a helper function taking the lifetime
            /// of a host value for the string, or by explicit annotation.
            #[inline]
            #[must_use]
            pub unsafe fn from_ptr_mut<'a>(p: *mut $uchar, len: usize) -> &'a mut Self {
                assert!(!p.is_null());
                let slice: *mut [$uchar] = slice::from_raw_parts_mut(p, len);
                &mut *(slice as *mut $ustr)
            }

            /// Constructs a wide string slice from a slice of character data.
            ///
            /// No checks are performed on the slice. It may be of any encoding and may contain
            /// invalid or malformed data for that encoding.
            #[inline]
            #[must_use]
            pub const fn from_slice(slice: &[$uchar]) -> &Self {
                let ptr: *const [$uchar] = slice;
                unsafe { &*(ptr as *const $ustr) }
            }

            /// Constructs a mutable wide string slice from a mutable slice of character data.
            ///
            /// No checks are performed on the slice. It may be of any encoding and may contain
            /// invalid or malformed data for that encoding.
            #[inline]
            #[must_use]
            pub fn from_slice_mut(slice: &mut [$uchar]) -> &mut Self {
                let ptr: *mut [$uchar] = slice;
                unsafe { &mut *(ptr as *mut $ustr) }
            }

            /// Copies the string reference to a new owned wide string.
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[inline]
            #[must_use]
            pub fn to_ustring(&self) -> $ustring {
                $ustring::from_vec(&self.inner)
            }

            /// Converts to a slice of the underlying elements of the string.
            #[inline]
            #[must_use]
            pub const fn as_slice(&self) -> &[$uchar] {
                &self.inner
            }

            /// Converts to a mutable slice of the underlying elements of the string.
            #[must_use]
            pub fn as_mut_slice(&mut self) -> &mut [$uchar] {
                &mut self.inner
            }

            /// Returns a raw pointer to the string.
            ///
            /// The caller must ensure that the string outlives the pointer this function returns,
            /// or else it will end up pointing to garbage.
            ///
            /// The caller must also ensure that the memory the pointer (non-transitively) points to
            /// is never written to (except inside an `UnsafeCell`) using this pointer or any
            /// pointer derived from it. If you need to mutate the contents of the string, use
            /// [`as_mut_ptr`][Self::as_mut_ptr].
            ///
            /// Modifying the container referenced by this string may cause its buffer to be
            /// reallocated, which would also make any pointers to it invalid.
            #[inline]
            #[must_use]
            pub const fn as_ptr(&self) -> *const $uchar {
                self.inner.as_ptr()
            }

            /// Returns an unsafe mutable raw pointer to the string.
            ///
            /// The caller must ensure that the string outlives the pointer this function returns,
            /// or else it will end up pointing to garbage.
            ///
            /// Modifying the container referenced by this string may cause its buffer to be
            /// reallocated, which would also make any pointers to it invalid.
            #[inline]
            #[must_use]
            pub fn as_mut_ptr(&mut self) -> *mut $uchar {
                self.inner.as_mut_ptr()
            }

            /// Returns the two raw pointers spanning the string slice.
            ///
            /// The returned range is half-open, which means that the end pointer points one past
            /// the last element of the slice. This way, an empty slice is represented by two equal
            /// pointers, and the difference between the two pointers represents the size of the
            /// slice.
            ///
            /// See [`as_ptr`][Self::as_ptr] for warnings on using these pointers. The end pointer
            /// requires extra caution, as it does not point to a valid element in the slice.
            ///
            /// This function is useful for interacting with foreign interfaces which use two
            /// pointers to refer to a range of elements in memory, as is common in C++.
            #[inline]
            #[must_use]
            pub fn as_ptr_range(&self) -> Range<*const $uchar> {
                self.inner.as_ptr_range()
            }

            /// Returns the two unsafe mutable pointers spanning the string slice.
            ///
            /// The returned range is half-open, which means that the end pointer points one past
            /// the last element of the slice. This way, an empty slice is represented by two equal
            /// pointers, and the difference between the two pointers represents the size of the
            /// slice.
            ///
            /// See [`as_mut_ptr`][Self::as_mut_ptr] for warnings on using these pointers. The end
            /// pointer requires extra caution, as it does not point to a valid element in the
            /// slice.
            ///
            /// This function is useful for interacting with foreign interfaces which use two
            /// pointers to refer to a range of elements in memory, as is common in C++.
            #[inline]
            #[must_use]
            pub fn as_mut_ptr_range(&mut self) -> Range<*mut $uchar> {
                self.inner.as_mut_ptr_range()
            }

            /// Returns the length of the string as number of elements (**not** number of bytes).
            #[inline]
            #[must_use]
            pub const fn len(&self) -> usize {
                self.inner.len()
            }

            /// Returns whether this string contains no data.
            #[inline]
            #[must_use]
            pub const fn is_empty(&self) -> bool {
                self.inner.is_empty()
            }

            /// Converts a boxed wide string slice into an owned wide string without copying or
            /// allocating.
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn into_ustring(self: Box<Self>) -> $ustring {
                let boxed = unsafe { Box::from_raw(Box::into_raw(self) as *mut [$uchar]) };
                $ustring {
                    inner: boxed.into_vec(),
                }
            }

            $(#[$display_meta])*
            #[inline]
            #[must_use]
            pub fn display(&self) -> Display<'_, $ustr> {
                Display { str: self }
            }

            /// Returns a subslice of the string.
            ///
            /// This is the non-panicking alternative to indexing the string. Returns [`None`]
            /// whenever equivalent indexing operation would panic.
            #[inline]
            #[must_use]
            pub fn get<I>(&self, i: I) -> Option<&Self>
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                self.inner.get(i).map(Self::from_slice)
            }

            /// Returns a mutable subslice of the string.
            ///
            /// This is the non-panicking alternative to indexing the string. Returns [`None`]
            /// whenever equivalent indexing operation would panic.
            #[inline]
            #[must_use]
            pub fn get_mut<I>(&mut self, i: I) -> Option<&mut Self>
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                self.inner.get_mut(i).map(Self::from_slice_mut)
            }

            /// Returns an unchecked subslice of the string.
            ///
            /// This is the unchecked alternative to indexing the string.
            ///
            /// # Safety
            ///
            /// Callers of this function are responsible that these preconditions are satisfied:
            ///
            /// - The starting index must not exceed the ending index;
            /// - Indexes must be within bounds of the original slice.
            ///
            /// Failing that, the returned string slice may reference invalid memory.
            #[inline]
            #[must_use]
            pub unsafe fn get_unchecked<I>(&self, i: I) -> &Self
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                Self::from_slice(self.inner.get_unchecked(i))
            }

            /// Returns aa mutable, unchecked subslice of the string.
            ///
            /// This is the unchecked alternative to indexing the string.
            ///
            /// # Safety
            ///
            /// Callers of this function are responsible that these preconditions are satisfied:
            ///
            /// - The starting index must not exceed the ending index;
            /// - Indexes must be within bounds of the original slice.
            ///
            /// Failing that, the returned string slice may reference invalid memory.
            #[inline]
            #[must_use]
            pub unsafe fn get_unchecked_mut<I>(&mut self, i: I) -> &mut Self
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                Self::from_slice_mut(self.inner.get_unchecked_mut(i))
            }

            /// Divide one string slice into two at an index.
            ///
            /// The argument, `mid`, should be an offset from the start of the string.
            ///
            /// The two slices returned go from the start of the string slice to `mid`, and from
            /// `mid` to the end of the string slice.
            ///
            /// To get mutable string slices instead, see the [`split_at_mut`][Self::split_at_mut]
            /// method.
            #[inline]
            #[must_use]
            pub fn split_at(&self, mid: usize) -> (&Self, &Self) {
                let split = self.inner.split_at(mid);
                (Self::from_slice(split.0), Self::from_slice(split.1))
            }

            /// Divide one mutable string slice into two at an index.
            ///
            /// The argument, `mid`, should be an offset from the start of the string.
            ///
            /// The two slices returned go from the start of the string slice to `mid`, and from
            /// `mid` to the end of the string slice.
            ///
            /// To get immutable string slices instead, see the [`split_at`][Self::split_at] method.
            #[inline]
            #[must_use]
            pub fn split_at_mut(&mut self, mid: usize) -> (&mut Self, &mut Self) {
                let split = self.inner.split_at_mut(mid);
                (Self::from_slice_mut(split.0), Self::from_slice_mut(split.1))
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
            pub fn repeat(&self, n: usize) -> $ustring {
                $ustring::from_vec(self.as_slice().repeat(n))
            }
        }

        impl AsMut<$ustr> for $ustr {
            #[inline]
            fn as_mut(&mut self) -> &mut $ustr {
                self
            }
        }

        impl AsMut<[$uchar]> for $ustr {
            #[inline]
            fn as_mut(&mut self) -> &mut [$uchar] {
                self.as_mut_slice()
            }
        }

        impl AsRef<$ustr> for $ustr {
            #[inline]
            fn as_ref(&self) -> &Self {
                self
            }
        }

        impl AsRef<[$uchar]> for $ustr {
            #[inline]
            fn as_ref(&self) -> &[$uchar] {
                self.as_slice()
            }
        }

        impl Default for &$ustr {
            #[inline]
            fn default() -> Self {
                $ustr::from_slice(&[])
            }
        }

        impl Default for &mut $ustr {
            #[inline]
            fn default() -> Self {
                $ustr::from_slice_mut(&mut [])
            }
        }

        impl<'a> From<&'a [$uchar]> for &'a $ustr {
            #[inline]
            fn from(value: &'a [$uchar]) -> Self {
                $ustr::from_slice(value)
            }
        }

        impl<'a> From<&'a mut [$uchar]> for &'a $ustr {
            #[inline]
            fn from(value: &'a mut [$uchar]) -> Self {
                $ustr::from_slice(value)
            }
        }

        impl<'a> From<&'a mut [$uchar]> for &'a mut $ustr {
            #[inline]
            fn from(value: &'a mut [$uchar]) -> Self {
                $ustr::from_slice_mut(value)
            }
        }

        impl<'a> From<&'a $ustr> for &'a [$uchar] {
            #[inline]
            fn from(value: &'a $ustr) -> Self {
                value.as_slice()
            }
        }

        impl<'a> From<&'a mut $ustr> for &'a mut [$uchar] {
            #[inline]
            fn from(value: &'a mut $ustr) -> Self {
                value.as_mut_slice()
            }
        }

        #[cfg(feature = "std")]
        impl From<&$ustr> for std::ffi::OsString {
            #[inline]
            fn from(s: &$ustr) -> std::ffi::OsString {
                s.to_os_string()
            }
        }

        impl<I> Index<I> for $ustr
        where
            I: SliceIndex<[$uchar], Output = [$uchar]>,
        {
            type Output = Self;

            #[inline]
            fn index(&self, index: I) -> &Self::Output {
                Self::from_slice(&self.inner[index])
            }
        }

        impl<I> IndexMut<I> for $ustr
        where
            I: SliceIndex<[$uchar], Output = [$uchar]>,
        {
            #[inline]
            fn index_mut(&mut self, index: I) -> &mut Self::Output {
                Self::from_slice_mut(&mut self.inner[index])
            }
        }

        impl PartialEq<$ustr> for &$ustr {
            #[inline]
            fn eq(&self, other: &$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<&$ustr> for $ustr {
            #[inline]
            fn eq(&self, other: &&$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ucstr> for $ustr {
            #[inline]
            fn eq(&self, other: &crate::$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<crate::$ucstr> for &$ustr {
            #[inline]
            fn eq(&self, other: &crate::$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<&crate::$ucstr> for $ustr {
            #[inline]
            fn eq(&self, other: &&crate::$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialOrd<crate::$ucstr> for $ustr {
            #[inline]
            fn partial_cmp(&self, other: &crate::$ucstr) -> Option<core::cmp::Ordering> {
                self.partial_cmp(other.as_ustr())
            }
        }
    };
}

ustr_common_impl! {
    /// 16-bit wide string slice with undefined encoding.
    ///
    /// [`U16Str`] is to [`U16String`][U16String] as [`OsStr`][std::ffi::OsStr] is to
    /// [`OsString`][std::ffi::OsString].
    ///
    /// [`U16Str`] are string slices that do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-16 data, they may be used for
    /// any wide encoded string. This is because [`U16Str`] is intended to be used with FFI
    /// functions, where proper encoding cannot be guaranteed. If you need string slices that are
    /// always valid UTF-16 strings, use [`Utf16Str`][crate::Utf16Str] instead.
    ///
    /// Because [`U16Str`] does not have a defined encoding, no restrictions are placed on mutating
    /// or indexing the slice. This means that even if the string contained properly encoded UTF-16
    /// or other encoding data, mutationing or indexing may result in malformed data. Convert to a
    /// [`Utf16Str`][crate::Utf16Str] if retaining proper UTF-16 encoding is desired.
    ///
    /// # FFI considerations
    ///
    /// [`U16Str`] is not aware of nul values and may or may not be nul-terminated. It is intended
    /// to be used with FFI functions that directly use string length, where the strings are known
    /// to have proper nul-termination already, or where strings are merely being passed through
    /// without modification.
    ///
    /// [`U16CStr`][crate::U16CStr] should be used instead if nul-aware strings are required.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U16Str`] outside of FFI is with the [`u16str!`][crate::u16str]
    /// macro to convert string literals into UTF-16 string slices at compile time:
    ///
    /// ```
    /// use widestring::u16str;
    /// let hello = u16str!("Hello, world!");
    /// ```
    ///
    /// You can also convert any [`u16`] slice directly:
    ///
    /// ```
    /// use widestring::{u16str, U16Str};
    ///
    /// let sparkle_heart = [0xd83d, 0xdc96];
    /// let sparkle_heart = U16Str::from_slice(&sparkle_heart);
    ///
    /// assert_eq!(u16str!("💖"), sparkle_heart);
    ///
    /// // This unpaired UTf-16 surrogate is invalid UTF-16, but is perfectly valid in U16Str
    /// let malformed_utf16 = [0x0, 0xd83d]; // Note that nul values are also valid an untouched
    /// let s = U16Str::from_slice(&malformed_utf16);
    ///
    /// assert_eq!(s.len(), 2);
    /// ```
    ///
    /// When working with a FFI, it is useful to create a [`U16Str`] from a pointer and a length:
    ///
    /// ```
    /// use widestring::{u16str, U16Str};
    ///
    /// let sparkle_heart = [0xd83d, 0xdc96];
    /// let sparkle_heart = unsafe {
    ///     U16Str::from_ptr(sparkle_heart.as_ptr(), sparkle_heart.len())
    /// };
    /// assert_eq!(u16str!("💖"), sparkle_heart);
    /// ```
    struct U16Str([u16]);

    type UString = U16String;
    type UCStr = U16CStr;

    /// Returns an object that implements [`Display`][std::fmt::Display] for printing
    /// strings that may contain non-Unicode data.
    ///
    /// This method assumes this string is intended to be UTF-16 encoding, but handles
    /// ill-formed UTF-16 sequences lossily. The returned struct implements
    /// the [`Display`][std::fmt::Display] trait in a way that decoding the string is lossy
    /// UTF-16 decoding but no heap allocations are performed, such as by
    /// [`to_string_lossy`][Self::to_string_lossy].
    ///
    /// By default, invalid Unicode data is replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�). If you wish
    /// to simply skip any invalid Uncode data and forego the replacement, you may use the
    /// [alternate formatting][std::fmt#sign0] with `{:#}`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::U16Str;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U16Str::from_slice(&[
    ///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
    /// ]);
    ///
    /// assert_eq!(format!("{}", s.display()),
    /// "𝄞mus�ic�"
    /// );
    /// ```
    ///
    /// Using alternate formatting style to skip invalid values entirely:
    ///
    /// ```
    /// use widestring::U16Str;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U16Str::from_slice(&[
    ///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
    /// ]);
    ///
    /// assert_eq!(format!("{:#}", s.display()),
    /// "𝄞music"
    /// );
    /// ```
    fn display() -> {}
}

ustr_common_impl! {
    /// 32-bit wide string slice with undefined encoding.
    ///
    /// [`U32Str`] is to [`U32String`][U32String] as [`OsStr`][std::ffi::OsStr] is to
    /// [`OsString`][std::ffi::OsString].
    ///
    /// [`U32Str`] are string slices that do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-32 data, they may be used for
    /// any wide encoded string. This is because [`U32Str`] is intended to be used with FFI
    /// functions, where proper encoding cannot be guaranteed. If you need string slices that are
    /// always valid UTF-32 strings, use [`Utf32Str`][crate::Utf32Str] instead.
    ///
    /// Because [`U32Str`] does not have a defined encoding, no restrictions are placed on mutating
    /// or indexing the slice. This means that even if the string contained properly encoded UTF-32
    /// or other encoding data, mutationing or indexing may result in malformed data. Convert to a
    /// [`Utf32Str`][crate::Utf32Str] if retaining proper UTF-32 encoding is desired.
    ///
    /// # FFI considerations
    ///
    /// [`U32Str`] is not aware of nul values and may or may not be nul-terminated. It is intended
    /// to be used with FFI functions that directly use string length, where the strings are known
    /// to have proper nul-termination already, or where strings are merely being passed through
    /// without modification.
    ///
    /// [`U32CStr`][crate::U32CStr] should be used instead if nul-aware strings are required.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U32Str`] outside of FFI is with the [`u32str!`][crate::u32str]
    /// macro to convert string literals into UTF-32 string slices at compile time:
    ///
    /// ```
    /// use widestring::u32str;
    /// let hello = u32str!("Hello, world!");
    /// ```
    ///
    /// You can also convert any [`u32`] slice directly:
    ///
    /// ```
    /// use widestring::{u32str, U32Str};
    ///
    /// let sparkle_heart = [0x1f496];
    /// let sparkle_heart = U32Str::from_slice(&sparkle_heart);
    ///
    /// assert_eq!(u32str!("💖"), sparkle_heart);
    ///
    /// // This UTf-16 surrogate is invalid UTF-32, but is perfectly valid in U32Str
    /// let malformed_utf32 = [0x0, 0xd83d]; // Note that nul values are also valid an untouched
    /// let s = U32Str::from_slice(&malformed_utf32);
    ///
    /// assert_eq!(s.len(), 2);
    /// ```
    ///
    /// When working with a FFI, it is useful to create a [`U32Str`] from a pointer and a length:
    ///
    /// ```
    /// use widestring::{u32str, U32Str};
    ///
    /// let sparkle_heart = [0x1f496];
    /// let sparkle_heart = unsafe {
    ///     U32Str::from_ptr(sparkle_heart.as_ptr(), sparkle_heart.len())
    /// };
    /// assert_eq!(u32str!("💖"), sparkle_heart);
    /// ```
    struct U32Str([u32]);

    type UString = U32String;
    type UCStr = U32CStr;

    /// Returns an object that implements [`Display`][std::fmt::Display] for printing
    /// strings that may contain non-Unicode data.
    ///
    /// This method assumes this string is intended to be UTF-32 encoding, but handles
    /// ill-formed UTF-32 sequences lossily. The returned struct implements
    /// the [`Display`][std::fmt::Display] trait in a way that decoding the string is lossy
    /// UTF-32 decoding but no heap allocations are performed, such as by
    /// [`to_string_lossy`][Self::to_string_lossy].
    ///
    /// By default, invalid Unicode data is replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�). If you wish
    /// to simply skip any invalid Uncode data and forego the replacement, you may use the
    /// [alternate formatting][std::fmt#sign0] with `{:#}`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use widestring::U32Str;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U32Str::from_slice(&[
    ///     0x1d11e, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
    /// ]);
    ///
    /// assert_eq!(format!("{}", s.display()),
    /// "𝄞mus�ic�"
    /// );
    /// ```
    ///
    /// Using alternate formatting style to skip invalid values entirely:
    ///
    /// ```
    /// use widestring::U32Str;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U32Str::from_slice(&[
    ///     0x1d11e, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
    /// ]);
    ///
    /// assert_eq!(format!("{:#}", s.display()),
    /// "𝄞music"
    /// );
    /// ```
    fn display() -> {}
}

impl U16Str {
    /// Decodes a string reference to an owned [`OsString`][std::ffi::OsString].
    ///
    /// This makes a string copy of the [`U16Str`]. Since [`U16Str`] makes no guarantees that its
    /// encoding is UTF-16 or that the data valid UTF-16, there is no guarantee that the resulting
    /// [`OsString`][std::ffi::OsString] will have a valid underlying encoding either.
    ///
    /// Note that the encoding of [`OsString`][std::ffi::OsString] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms (such as
    /// windows) no changes to the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// use std::ffi::OsString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16String::from_str(s);
    /// // Create an OsString from the wide string
    /// let osstr = wstr.to_os_string();
    ///
    /// assert_eq!(osstr, OsString::from(s));
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    #[must_use]
    pub fn to_os_string(&self) -> std::ffi::OsString {
        crate::platform::os_from_wide(&self.inner)
    }

    /// Decodes this string to a [`String`] if it contains valid UTF-16 data.
    ///
    /// This method assumes this string is encoded as UTF-16 and attempts to decode it as such.
    ///
    /// # Failures
    ///
    /// Returns an error if the string contains any invalid UTF-16 data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16String::from_str(s);
    /// // Create a regular string from the wide string
    /// let s2 = wstr.to_string().unwrap();
    ///
    /// assert_eq!(s2, s);
    /// ```
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[inline]
    pub fn to_string(&self) -> Result<String, Utf16Error> {
        // Perform conversion ourselves to use our own error types with additional info
        let mut s = String::with_capacity(self.len());
        for (index, result) in self.chars().enumerate() {
            let c = result.map_err(|e| Utf16Error::empty(index, e))?;
            s.push(c);
        }
        Ok(s)
    }

    /// Decodes the string to a [`String`] even if it is invalid UTF-16 data.
    ///
    /// This method assumes this string is encoded as UTF-16 and attempts to decode it as such. Any
    /// invalid sequences are replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which looks like this:
    /// �
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16String::from_str(s);
    /// // Create a regular string from the wide string
    /// let lossy = wstr.to_string_lossy();
    ///
    /// assert_eq!(lossy, s);
    /// ```
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[inline]
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.inner)
    }

    /// Returns an iterator over the [`char`][prim@char]s of a string slice.
    ///
    /// As this string has no defined encoding, this method assumes the string is UTF-16. Since it
    /// may consist of invalid UTF-16, the iterator returned by this method
    /// is an iterator over `Result<char, DecodeUtf16Error>` instead of [`char`][prim@char]s
    /// directly. If you would like a lossy iterator over [`chars`][prim@char]s directly, instead
    /// use [`chars_lossy`][Self::chars_lossy].
    ///
    /// It's important to remember that [`char`][prim@char] represents a Unicode Scalar Value, and
    /// may not match your idea of what a 'character' is. Iteration over grapheme clusters may be
    /// what you actually want. That functionality is not provided by by this crate.
    #[inline]
    #[must_use]
    pub fn chars(&self) -> CharsUtf16<'_> {
        CharsUtf16::new(self.as_slice())
    }

    /// Returns a lossy iterator over the [`char`][prim@char]s of a string slice.
    ///
    /// As this string has no defined encoding, this method assumes the string is UTF-16. Since it
    /// may consist of invalid UTF-16, the iterator returned by this method will replace unpaired
    /// surrogates with
    /// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�). This is a lossy
    /// version of [`chars`][Self::chars].
    ///
    /// It's important to remember that [`char`][prim@char] represents a Unicode Scalar Value, and
    /// may not match your idea of what a 'character' is. Iteration over grapheme clusters may be
    /// what you actually want. That functionality is not provided by by this crate.
    #[inline]
    #[must_use]
    pub fn chars_lossy(&self) -> CharsLossyUtf16<'_> {
        CharsLossyUtf16::new(self.as_slice())
    }

    /// Returns an iterator over the chars of a string slice, and their positions.
    ///
    /// As this string has no defined encoding, this method assumes the string is UTF-16. Since it
    /// may consist of invalid UTF-16, the iterator returned by this method is an iterator over
    /// `Result<char, DecodeUtf16Error>` as well as their positions, instead of
    /// [`char`][prim@char]s directly. If you would like a lossy indices iterator over
    /// [`chars`][prim@char]s directly, instead use
    /// [`char_indices_lossy`][Self::char_indices_lossy].
    ///
    /// The iterator yields tuples. The position is first, the [`char`][prim@char] is second.
    #[inline]
    #[must_use]
    pub fn char_indices(&self) -> CharIndicesUtf16<'_> {
        CharIndicesUtf16::new(self.as_slice())
    }

    /// Returns a lossy iterator over the chars of a string slice, and their positions.
    ///
    /// As this string slice may consist of invalid UTF-16, the iterator returned by this method
    /// will replace unpaired surrogates with
    /// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�), as well as the
    /// positions of all characters. This is a lossy version of
    /// [`char_indices`][Self::char_indices].
    ///
    /// The iterator yields tuples. The position is first, the [`char`][prim@char] is second.
    #[inline]
    #[must_use]
    pub fn char_indices_lossy(&self) -> CharIndicesLossyUtf16<'_> {
        CharIndicesLossyUtf16::new(self.as_slice())
    }

    /// Returns an iterator over the lines of a [`U16Str`], as string slices.
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
    /// use widestring::{u16str};
    ///
    /// let text = u16str!("foo\r\nbar\n\nbaz\r");
    /// let mut lines = text.lines_lossy();
    ///
    /// assert_eq!(Some(u16str!("foo")), lines.next());
    /// assert_eq!(Some(u16str!("bar")), lines.next());
    /// assert_eq!(Some(u16str!("")), lines.next());
    /// // Trailing carriage return is included in the last line
    /// assert_eq!(Some(u16str!("baz\r")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    ///
    /// The final line does not require any ending:
    ///
    /// ```
    /// use widestring::{u16str};
    ///
    /// let text = u16str!("foo\nbar\n\r\nbaz");
    /// let mut lines = text.lines_lossy();
    ///
    /// assert_eq!(Some(u16str!("foo")), lines.next());
    /// assert_eq!(Some(u16str!("bar")), lines.next());
    /// assert_eq!(Some(u16str!("")), lines.next());
    /// assert_eq!(Some(u16str!("baz")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    pub fn lines_lossy(&self) -> Lines<'_, Self, CharIndicesLossyUtf16<'_>> {
        Lines::new(self, self.len(), self.char_indices_lossy())
    }
}

impl U32Str {
    /// Constructs a [`U32Str`] from a [`char`][prim@char] pointer and a length.
    ///
    /// The `len` argument is the number of `char` elements, **not** the number of bytes. No copying
    /// or allocation is performed, the resulting value is a direct reference to the pointer bytes.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len`
    /// elements.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts].
    /// In particular, the returned string reference *must not be mutated* for the duration of
    /// lifetime `'a`, except inside an [`UnsafeCell`][std::cell::UnsafeCell].
    ///
    /// # Panics
    ///
    /// This function panics if `p` is null.
    ///
    /// # Caveat
    ///
    /// The lifetime for the returned string is inferred from its usage. To prevent accidental
    /// misuse, it's suggested to tie the lifetime to whichever source lifetime is safe in the
    /// context, such as by providing a helper function taking the lifetime of a host value for the
    /// string, or by explicit annotation.
    #[inline]
    #[must_use]
    pub unsafe fn from_char_ptr<'a>(p: *const char, len: usize) -> &'a Self {
        Self::from_ptr(p as *const u32, len)
    }

    /// Constructs a mutable [`U32Str`] from a mutable [`char`][prim@char] pointer and a length.
    ///
    /// The `len` argument is the number of `char` elements, **not** the number of bytes. No copying
    /// or allocation is performed, the resulting value is a direct reference to the pointer bytes.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len`
    /// elements.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts_mut].
    ///
    /// # Panics
    ///
    /// This function panics if `p` is null.
    ///
    /// # Caveat
    ///
    /// The lifetime for the returned string is inferred from its usage. To prevent accidental
    /// misuse, it's suggested to tie the lifetime to whichever source lifetime is safe in the
    /// context, such as by providing a helper function taking the lifetime of a host value for the
    /// string, or by explicit annotation.
    #[inline]
    #[must_use]
    pub unsafe fn from_char_ptr_mut<'a>(p: *mut char, len: usize) -> &'a mut Self {
        Self::from_ptr_mut(p as *mut u32, len)
    }

    /// Constructs a [`U32Str`] from a [`char`][prim@char] slice.
    ///
    /// No checks are performed on the slice.
    #[inline]
    #[must_use]
    pub fn from_char_slice(slice: &[char]) -> &Self {
        let ptr: *const [char] = slice;
        unsafe { &*(ptr as *const Self) }
    }

    /// Constructs a mutable [`U32Str`] from a mutable [`char`][prim@char] slice.
    ///
    /// No checks are performed on the slice.
    #[inline]
    #[must_use]
    pub fn from_char_slice_mut(slice: &mut [char]) -> &mut Self {
        let ptr: *mut [char] = slice;
        unsafe { &mut *(ptr as *mut Self) }
    }

    /// Decodes a string to an owned [`OsString`][std::ffi::OsString].
    ///
    /// This makes a string copy of the [`U16Str`]. Since [`U16Str`] makes no guarantees that its
    /// encoding is UTF-16 or that the data valid UTF-16, there is no guarantee that the resulting
    /// [`OsString`][std::ffi::OsString] will have a valid underlying encoding either.
    ///
    /// Note that the encoding of [`OsString`][std::ffi::OsString] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms no changes to
    /// the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// use std::ffi::OsString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32String::from_str(s);
    /// // Create an OsString from the wide string
    /// let osstr = wstr.to_os_string();
    ///
    /// assert_eq!(osstr, OsString::from(s));
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    #[must_use]
    pub fn to_os_string(&self) -> std::ffi::OsString {
        self.to_string_lossy().into()
    }

    /// Decodes the string to a [`String`] if it contains valid UTF-32 data.
    ///
    /// This method assumes this string is encoded as UTF-32 and attempts to decode it as such.
    ///
    /// # Failures
    ///
    /// Returns an error if the string contains any invalid UTF-32 data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32String::from_str(s);
    /// // Create a regular string from the wide string
    /// let s2 = wstr.to_string().unwrap();
    ///
    /// assert_eq!(s2, s);
    /// ```
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    pub fn to_string(&self) -> Result<String, Utf32Error> {
        let mut s = String::with_capacity(self.len());
        for (index, result) in self.chars().enumerate() {
            let c = result.map_err(|e| Utf32Error::empty(index, e))?;
            s.push(c);
        }
        Ok(s)
    }

    /// Decodes the string reference to a [`String`] even if it is invalid UTF-32 data.
    ///
    /// This method assumes this string is encoded as UTF-16 and attempts to decode it as such. Any
    /// invalid sequences are replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which looks like this:
    /// �
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32String;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32String::from_str(s);
    /// // Create a regular string from the wide string
    /// let lossy = wstr.to_string_lossy();
    ///
    /// assert_eq!(lossy, s);
    /// ```
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        let chars: Vec<char> = self
            .inner
            .iter()
            .map(|&c| char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        let size = chars.iter().map(|c| c.len_utf8()).sum();
        let mut vec = alloc::vec![0; size];
        let mut i = 0;
        for c in chars {
            c.encode_utf8(&mut vec[i..]);
            i += c.len_utf8();
        }
        unsafe { String::from_utf8_unchecked(vec) }
    }

    /// Returns an iterator over the [`char`][prim@char]s of a string slice.
    ///
    /// As this string has no defined encoding, this method assumes the string is UTF-32. Since it
    /// may consist of invalid UTF-32, the iterator returned by this method
    /// is an iterator over `Result<char, DecodeUtf32Error>` instead of [`char`][prim@char]s
    /// directly. If you would like a lossy iterator over [`chars`][prim@char]s directly, instead
    /// use [`chars_lossy`][Self::chars_lossy].
    ///
    /// It's important to remember that [`char`][prim@char] represents a Unicode Scalar Value, and
    /// may not match your idea of what a 'character' is. Iteration over grapheme clusters may be
    /// what you actually want. That functionality is not provided by by this crate.
    #[inline]
    #[must_use]
    pub fn chars(&self) -> CharsUtf32<'_> {
        CharsUtf32::new(self.as_slice())
    }

    /// Returns a lossy iterator over the [`char`][prim@char]s of a string slice.
    ///
    /// As this string has no defined encoding, this method assumes the string is UTF-32. Since it
    /// may consist of invalid UTF-32, the iterator returned by this method will replace unpaired
    /// surrogates with
    /// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�). This is a lossy
    /// version of [`chars`][Self::chars].
    ///
    /// It's important to remember that [`char`][prim@char] represents a Unicode Scalar Value, and
    /// may not match your idea of what a 'character' is. Iteration over grapheme clusters may be
    /// what you actually want. That functionality is not provided by by this crate.
    #[inline]
    #[must_use]
    pub fn chars_lossy(&self) -> CharsLossyUtf32<'_> {
        CharsLossyUtf32::new(self.as_slice())
    }

    /// Returns an iterator over the chars of a string slice, and their positions.
    ///
    /// As this string has no defined encoding, this method assumes the string is UTF-32. Since it
    /// may consist of invalid UTF-32, the iterator returned by this method is an iterator over
    /// `Result<char, DecodeUtf32Error>` as well as their positions, instead of
    /// [`char`][prim@char]s directly. If you would like a lossy indices iterator over
    /// [`chars`][prim@char]s directly, instead use
    /// [`char_indices_lossy`][Self::char_indices_lossy].
    ///
    /// The iterator yields tuples. The position is first, the [`char`][prim@char] is second.
    #[inline]
    #[must_use]
    pub fn char_indices(&self) -> CharIndicesUtf32<'_> {
        CharIndicesUtf32::new(self.as_slice())
    }

    /// Returns a lossy iterator over the chars of a string slice, and their positions.
    ///
    /// As this string slice may consist of invalid UTF-32, the iterator returned by this method
    /// will replace invalid values with
    /// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�), as well as the
    /// positions of all characters. This is a lossy version of
    /// [`char_indices`][Self::char_indices].
    ///
    /// The iterator yields tuples. The position is first, the [`char`][prim@char] is second.
    #[inline]
    #[must_use]
    pub fn char_indices_lossy(&self) -> CharIndicesLossyUtf32<'_> {
        CharIndicesLossyUtf32::new(self.as_slice())
    }

    /// Returns an iterator over the lines of a [`U32Str`], as string slices.
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
    /// use widestring::{u32str};
    ///
    /// let text = u32str!("foo\r\nbar\n\nbaz\r");
    /// let mut lines = text.lines_lossy();
    ///
    /// assert_eq!(Some(u32str!("foo")), lines.next());
    /// assert_eq!(Some(u32str!("bar")), lines.next());
    /// assert_eq!(Some(u32str!("")), lines.next());
    /// // Trailing carriage return is included in the last line
    /// assert_eq!(Some(u32str!("baz\r")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    ///
    /// The final line does not require any ending:
    ///
    /// ```
    /// use widestring::{u32str};
    ///
    /// let text = u32str!("foo\nbar\n\r\nbaz");
    /// let mut lines = text.lines_lossy();
    ///
    /// assert_eq!(Some(u32str!("foo")), lines.next());
    /// assert_eq!(Some(u32str!("bar")), lines.next());
    /// assert_eq!(Some(u32str!("")), lines.next());
    /// assert_eq!(Some(u32str!("baz")), lines.next());
    ///
    /// assert_eq!(None, lines.next());
    /// ```
    pub fn lines_lossy(&self) -> Lines<'_, Self, CharIndicesLossyUtf32<'_>> {
        Lines::new(self, self.len(), self.char_indices_lossy())
    }
}

impl core::fmt::Debug for U16Str {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u16(self.as_slice(), f)
    }
}

impl core::fmt::Debug for U32Str {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u32(self.as_slice(), f)
    }
}

impl<'a> From<&'a [char]> for &'a U32Str {
    #[inline]
    fn from(value: &'a [char]) -> Self {
        U32Str::from_char_slice(value)
    }
}

impl<'a> From<&'a mut [char]> for &'a mut U32Str {
    #[inline]
    fn from(value: &'a mut [char]) -> Self {
        U32Str::from_char_slice_mut(value)
    }
}

/// Alias for [`U16Str`] or [`U32Str`] depending on platform. Intended to match typical C `wchar_t`
/// size on platform.
#[cfg(not(windows))]
pub type WideStr = U32Str;

/// Alias for [`U16Str`] or [`U32Str`] depending on platform. Intended to match typical C `wchar_t`
/// size on platform.
#[cfg(windows)]
pub type WideStr = U16Str;

/// Helper struct for printing wide string values with [`format!`] and `{}`.
///
/// A wide string might contain ill-formed UTF encoding. This struct implements the
/// [`Display`][std::fmt::Display] trait in a way that decoding the string is lossy but no heap
/// allocations are performed, such as by [`to_string_lossy`][U16Str::to_string_lossy]. It is
/// created by the [`display`][U16Str::display] method on [`U16Str`] and [`U32Str`].
///
/// By default, invalid Unicode data is replaced with
/// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�). If you wish to simply
/// skip any invalid Uncode data and forego the replacement, you may use the
/// [alternate formatting][std::fmt#sign0] with `{:#}`.
pub struct Display<'a, S: ?Sized> {
    str: &'a S,
}

impl core::fmt::Debug for Display<'_, U16Str> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.str, f)
    }
}

impl core::fmt::Debug for Display<'_, U32Str> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.str, f)
    }
}

impl core::fmt::Display for Display<'_, U16Str> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for c in crate::decode_utf16_lossy(self.str.as_slice().iter().copied()) {
            // Allow alternate {:#} format which skips replacment chars entirely
            if c != core::char::REPLACEMENT_CHARACTER || !f.alternate() {
                f.write_char(c)?;
            }
        }
        Ok(())
    }
}

impl core::fmt::Display for Display<'_, U32Str> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for c in crate::decode_utf32_lossy(self.str.as_slice().iter().copied()) {
            // Allow alternate {:#} format which skips replacment chars entirely
            if c != core::char::REPLACEMENT_CHARACTER || !f.alternate() {
                f.write_char(c)?;
            }
        }
        Ok(())
    }
}
