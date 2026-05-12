//! C-style owned, growable wide strings.
//!
//! This module contains wide C strings and related types.

use crate::{error::ContainsNul, U16CStr, U16Str, U16String, U32CStr, U32Str, U32String};
#[allow(unused_imports)]
use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    vec::Vec,
};
use core::{
    borrow::{Borrow, BorrowMut},
    cmp,
    mem::{self, ManuallyDrop},
    ops::{Deref, DerefMut, Index},
    ptr,
    slice::{self, SliceIndex},
};

macro_rules! ucstring_common_impl {
    {
        $(#[$ucstring_meta:meta])*
        struct $ucstring:ident([$uchar:ty]);
        type UCStr = $ucstr:ident;
        type UString = $ustring:ident;
        type UStr = $ustr:ident;
        $(#[$from_vec_meta:meta])*
        fn from_vec() -> {}
        $(#[$from_vec_truncate_meta:meta])*
        fn from_vec_truncate() -> {}
        $(#[$into_boxed_ucstr_meta:meta])*
        fn into_boxed_ucstr() -> {}
    } => {
        $(#[$ucstring_meta])*
        #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $ucstring {
            pub(crate) inner: Box<[$uchar]>,
        }

        impl $ucstring {
            /// The nul terminator character value.
            pub const NUL_TERMINATOR: $uchar = 0;

            /// Constructs a new empty wide C string.
            #[inline]
            #[must_use]
            pub fn new() -> Self {
                unsafe { Self::from_vec_unchecked(Vec::new()) }
            }

            $(#[$from_vec_meta])*
            pub fn from_vec(v: impl Into<Vec<$uchar>>) -> Result<Self, ContainsNul<$uchar>> {
                let v = v.into();
                // Check for nul vals, ignoring nul terminator
                match v.iter().position(|&val| val == Self::NUL_TERMINATOR) {
                    None => Ok(unsafe { Self::from_vec_unchecked(v) }),
                    Some(pos) if pos == v.len() - 1 => Ok(unsafe { Self::from_vec_unchecked(v) }),
                    Some(pos) => Err(ContainsNul::new(pos, v)),
                }
            }

            $(#[$from_vec_truncate_meta])*
            #[must_use]
            pub fn from_vec_truncate(v: impl Into<Vec<$uchar>>) -> Self {
                let mut v = v.into();
                // Check for nul vals
                if let Some(pos) = v.iter().position(|&val| val == Self::NUL_TERMINATOR) {
                    v.truncate(pos + 1);
                }
                unsafe { Self::from_vec_unchecked(v) }
            }

            /// Constructs a wide C string from a vector without checking for interior nul values.
            ///
            /// A terminating nul value will be appended if the vector does not already have a
            /// terminating nul.
            ///
            /// # Safety
            ///
            /// This method is equivalent to [`from_vec`][Self::from_vec] except that no runtime
            /// assertion is made that `v` contains no interior nul values. Providing a vector with
            /// any nul values that are not the last value in the vector will result in an invalid
            /// C string.
            #[must_use]
            pub unsafe fn from_vec_unchecked(v: impl Into<Vec<$uchar>>) -> Self {
                let mut v = v.into();
                match v.last() {
                    None => v.push(Self::NUL_TERMINATOR),
                    Some(&c) if c != Self::NUL_TERMINATOR => v.push(Self::NUL_TERMINATOR),
                    Some(_) => (),
                }
                Self {
                    inner: v.into_boxed_slice(),
                }
            }

            /// Constructs a wide C string from anything that can be converted to a wide string
            /// slice.
            ///
            /// The string will be scanned for invalid interior nul values.
            ///
            /// # Errors
            ///
            /// This function will return an error if the data contains a nul value that is not the
            /// terminating nul.
            /// The returned error will contain a [`Vec`] as well as the position of the nul value.
            #[inline]
            pub fn from_ustr(s: impl AsRef<$ustr>) -> Result<Self, ContainsNul<$uchar>> {
                Self::from_vec(s.as_ref().as_slice())
            }

            /// Constructs a wide C string from anything that can be converted to a wide string
            /// slice, truncating at the first nul terminator.
            ///
            /// The string will be truncated at the first nul value in the string.
            #[inline]
            #[must_use]
            pub fn from_ustr_truncate(s: impl AsRef<$ustr>) -> Self {
                Self::from_vec_truncate(s.as_ref().as_slice())
            }

            /// Constructs a wide C string from anything that can be converted to a wide string
            /// slice, without scanning for invalid nul values.
            ///
            /// # Safety
            ///
            /// This method is equivalent to [`from_ustr`][Self::from_ustr] except that no runtime
            /// assertion is made that `v` contains no interior nul values. Providing a string with
            /// any nul values that are not the last value in the vector will result in an invalid
            /// C string.
            #[inline]
            #[must_use]
            pub unsafe fn from_ustr_unchecked(s: impl AsRef<$ustr>) -> Self {
                Self::from_vec_unchecked(s.as_ref().as_slice())
            }

            /// Constructs a new wide C string copied from a nul-terminated string pointer.
            ///
            /// This will scan for nul values beginning with `p`. The first nul value will be used
            /// as the nul terminator for the string, similar to how libc string functions such as
            /// `strlen` work.
            ///
            /// If you wish to avoid copying the string pointer, use [`U16CStr::from_ptr_str`] or
            /// [`U32CStr::from_ptr_str`] instead.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid or
            /// has a nul terminator, and the function could scan past the underlying buffer.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts].
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
            pub unsafe fn from_ptr_str(p: *const $uchar) -> Self {
                $ucstr::from_ptr_str(p).to_ucstring()
            }

            /// Constructs a wide C string copied from a pointer and a length, checking for invalid
            /// interior nul values.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes, and does
            /// **not** include the nul terminator of the string. If `len` is `0`, `p` is allowed to
            /// be a null pointer.
            ///
            /// The resulting string will always be nul-terminated even if the pointer data is not.
            ///
            /// # Errors
            ///
            /// This will scan the pointer string for an interior nul value and error if one is
            /// found. To avoid scanning for interior nuls,
            /// [`from_ptr_unchecked`][Self::from_ptr_unchecked] may be used instead.
            /// The returned error will contain a [`Vec`] as well as the position of the nul value.
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
            pub unsafe fn from_ptr(
                p: *const $uchar,
                len: usize,
            ) -> Result<Self, ContainsNul<$uchar>> {
                if len == 0 {
                    return Ok(Self::default());
                }
                assert!(!p.is_null());
                let slice = slice::from_raw_parts(p, len);
                Self::from_vec(slice)
            }

            /// Constructs a wide C string copied from a pointer and a length, truncating at the
            /// first nul terminator.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes. This will
            /// scan for nul values beginning with `p` until offset `len`. The first nul value will
            /// be used as the nul terminator for the string, ignoring any remaining values left
            /// before `len`. If no nul value is found, the whole string of length `len` is used,
            /// and a new nul-terminator will be added to the resulting string. If `len` is `0`, `p`
            /// is allowed to be a null pointer.
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
            pub unsafe fn from_ptr_truncate(p: *const $uchar, len: usize) -> Self {
                if len == 0 {
                    return Self::default();
                }
                assert!(!p.is_null());
                let slice = slice::from_raw_parts(p, len);
                Self::from_vec_truncate(slice)
            }

            /// Constructs a wide C string copied from a pointer and a length without checking for
            /// any nul values.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes, and does
            /// **not** include the nul terminator of the string. If `len` is `0`, `p` is allowed to
            /// be a null pointer.
            ///
            /// The resulting string will always be nul-terminated even if the pointer data is not.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len` elements.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts].
            ///
            /// The interior values of the pointer are not scanned for nul. Any interior nul values
            /// or will result in an invalid C string.
            ///
            /// # Panics
            ///
            /// Panics if `len` is greater than 0 but `p` is a null pointer.
            #[must_use]
            pub unsafe fn from_ptr_unchecked(p: *const $uchar, len: usize) -> Self {
                if len == 0 {
                    return Self::default();
                }
                assert!(!p.is_null());
                let slice = slice::from_raw_parts(p, len);
                Self::from_vec_unchecked(slice)
            }

            /// Converts to a wide C string slice.
            #[inline]
            #[must_use]
            pub fn as_ucstr(&self) -> &$ucstr {
                $ucstr::from_inner(&self.inner)
            }

            /// Converts to a mutable wide C string slice.
            #[inline]
            #[must_use]
            pub fn as_mut_ucstr(&mut self) -> &mut $ucstr {
                $ucstr::from_inner_mut(&mut self.inner)
            }

            /// Converts this string into a wide string without a nul terminator.
            ///
            /// The resulting string will **not** contain a nul-terminator, and will contain no
            /// other nul values.
            #[inline]
            #[must_use]
            pub fn into_ustring(self) -> $ustring {
                $ustring::from_vec(self.into_vec())
            }

            /// Converts this string into a wide string with a nul terminator.
            ///
            /// The resulting vector will contain a nul-terminator and no interior nul values.
            #[inline]
            #[must_use]
            pub fn into_ustring_with_nul(self) -> $ustring {
                $ustring::from_vec(self.into_vec_with_nul())
            }

            /// Converts the string into a [`Vec`] without a nul terminator, consuming the string in
            /// the process.
            ///
            /// The resulting vector will **not** contain a nul-terminator, and will contain no
            /// other nul values.
            #[inline]
            #[must_use]
            pub fn into_vec(self) -> Vec<$uchar> {
                let mut v = self.into_inner().into_vec();
                v.pop();
                v
            }

            /// Converts the string into a [`Vec`], consuming the string in the process.
            ///
            /// The resulting vector will contain a nul-terminator and no interior nul values.
            #[inline]
            #[must_use]
            pub fn into_vec_with_nul(self) -> Vec<$uchar> {
                self.into_inner().into_vec()
            }

            /// Transfers ownership of the string to a C caller.
            ///
            /// # Safety
            ///
            /// The pointer _must_ be returned to Rust and reconstituted using
            /// [`from_raw`][Self::from_raw] to be properly deallocated. Specifically, one should
            /// _not_ use the standard C `free` function to deallocate this string. Failure to call
            /// [`from_raw`][Self::from_raw] will lead to a memory leak.
            #[inline]
            #[must_use]
            pub fn into_raw(self) -> *mut $uchar {
                Box::into_raw(self.into_inner()) as *mut $uchar
            }

            /// Retakes ownership of a wide C string that was transferred to C.
            ///
            /// This should only be used in combination with [`into_raw`][Self::into_raw]. To
            /// construct a new wide C string from a pointer, use
            /// [`from_ptr_str`][Self::from_ptr_str].
            ///
            /// # Safety
            ///
            /// This should only ever be called with a pointer that was earlier obtained by calling
            /// [`into_raw`][Self::into_raw]. Additionally, the length of the string will be
            /// recalculated from the pointer by scanning for the nul-terminator.
            ///
            /// # Panics
            ///
            /// Panics if `p` is a null pointer.
            #[must_use]
            pub unsafe fn from_raw(p: *mut $uchar) -> Self {
                assert!(!p.is_null());
                let mut i: isize = 0;
                while *p.offset(i) != Self::NUL_TERMINATOR {
                    i += 1;
                }
                let slice = slice::from_raw_parts_mut(p, i as usize + 1);
                Self {
                    inner: Box::from_raw(slice),
                }
            }

            $(#[$into_boxed_ucstr_meta])*
            #[inline]
            #[must_use]
            pub fn into_boxed_ucstr(self) -> Box<$ucstr> {
                unsafe { Box::from_raw(Box::into_raw(self.into_inner()) as *mut $ucstr) }
            }

            /// Bypass "move out of struct which implements [`Drop`] trait" restriction.
            fn into_inner(self) -> Box<[$uchar]> {
                let v = ManuallyDrop::new(self);
                unsafe {
                    ptr::read(&v.inner)
                }
            }
        }

        impl AsMut<$ucstr> for $ucstring {
            fn as_mut(&mut self) -> &mut $ucstr {
                self.as_mut_ucstr()
            }
        }

        impl AsRef<$ucstr> for $ucstring {
            #[inline]
            fn as_ref(&self) -> &$ucstr {
                self.as_ucstr()
            }
        }

        impl AsRef<[$uchar]> for $ucstring {
            #[inline]
            fn as_ref(&self) -> &[$uchar] {
                self.as_slice()
            }
        }

        impl AsRef<$ustr> for $ucstring {
            #[inline]
            fn as_ref(&self) -> &$ustr {
                self.as_ustr()
            }
        }

        impl Borrow<$ucstr> for $ucstring {
            #[inline]
            fn borrow(&self) -> &$ucstr {
                self.as_ucstr()
            }
        }

        impl BorrowMut<$ucstr> for $ucstring {
            #[inline]
            fn borrow_mut(&mut self) -> &mut $ucstr {
                self.as_mut_ucstr()
            }
        }

        impl Default for $ucstring {
            #[inline]
            fn default() -> Self {
                unsafe { Self::from_vec_unchecked(Vec::new()) }
            }
        }

        impl Deref for $ucstring {
            type Target = $ucstr;

            #[inline]
            fn deref(&self) -> &$ucstr {
                self.as_ucstr()
            }
        }

        impl DerefMut for $ucstring {
            #[inline]
            fn deref_mut(&mut self) -> &mut Self::Target {
                self.as_mut_ucstr()
            }
        }

        // Turns this `UCString` into an empty string to prevent
        // memory unsafe code from working by accident. Inline
        // to prevent LLVM from optimizing it away in debug builds.
        impl Drop for $ucstring {
            #[inline]
            fn drop(&mut self) {
                unsafe {
                    *self.inner.get_unchecked_mut(0) = Self::NUL_TERMINATOR;
                }
            }
        }

        impl From<$ucstring> for Vec<$uchar> {
            #[inline]
            fn from(value: $ucstring) -> Self {
                value.into_vec()
            }
        }

        impl<'a> From<$ucstring> for Cow<'a, $ucstr> {
            #[inline]
            fn from(s: $ucstring) -> Cow<'a, $ucstr> {
                Cow::Owned(s)
            }
        }

        #[cfg(feature = "std")]
        impl From<$ucstring> for std::ffi::OsString {
            #[inline]
            fn from(s: $ucstring) -> std::ffi::OsString {
                s.to_os_string()
            }
        }

        impl From<$ucstring> for $ustring {
            #[inline]
            fn from(s: $ucstring) -> Self {
                s.to_ustring()
            }
        }

        impl<'a, T: ?Sized + AsRef<$ucstr>> From<&'a T> for $ucstring {
            #[inline]
            fn from(s: &'a T) -> Self {
                s.as_ref().to_ucstring()
            }
        }

        impl<'a> From<&'a $ucstr> for Cow<'a, $ucstr> {
            #[inline]
            fn from(s: &'a $ucstr) -> Cow<'a, $ucstr> {
                Cow::Borrowed(s)
            }
        }

        impl From<Box<$ucstr>> for $ucstring {
            #[inline]
            fn from(s: Box<$ucstr>) -> Self {
                s.into_ucstring()
            }
        }

        impl From<$ucstring> for Box<$ucstr> {
            #[inline]
            fn from(s: $ucstring) -> Box<$ucstr> {
                s.into_boxed_ucstr()
            }
        }

        impl<I> Index<I> for $ucstring
        where
            I: SliceIndex<[$uchar], Output = [$uchar]>,
        {
            type Output = $ustr;

            #[inline]
            fn index(&self, index: I) -> &Self::Output {
                &self.as_ucstr()[index]
            }
        }

        impl PartialEq<$ustr> for $ucstring {
            #[inline]
            fn eq(&self, other: &$ustr) -> bool {
                self.as_ucstr() == other
            }
        }

        impl PartialEq<$ucstr> for $ucstring {
            #[inline]
            fn eq(&self, other: &$ucstr) -> bool {
                self.as_ucstr() == other
            }
        }

        impl<'a> PartialEq<&'a $ustr> for $ucstring {
            #[inline]
            fn eq(&self, other: &&'a $ustr) -> bool {
                self.as_ucstr() == *other
            }
        }

        impl<'a> PartialEq<&'a $ucstr> for $ucstring {
            #[inline]
            fn eq(&self, other: &&'a $ucstr) -> bool {
                self.as_ucstr() == *other
            }
        }

        impl<'a> PartialEq<Cow<'a, $ustr>> for $ucstring {
            #[inline]
            fn eq(&self, other: &Cow<'a, $ustr>) -> bool {
                self.as_ucstr() == other.as_ref()
            }
        }

        impl<'a> PartialEq<Cow<'a, $ucstr>> for $ucstring {
            #[inline]
            fn eq(&self, other: &Cow<'a, $ucstr>) -> bool {
                self.as_ucstr() == other.as_ref()
            }
        }

        impl PartialEq<$ustring> for $ucstring {
            #[inline]
            fn eq(&self, other: &$ustring) -> bool {
                self.as_ustr() == other.as_ustr()
            }
        }

        impl PartialEq<$ucstring> for $ustr {
            #[inline]
            fn eq(&self, other: &$ucstring) -> bool {
                self == other.as_ustr()
            }
        }

        impl PartialEq<$ucstring> for $ucstr {
            #[inline]
            fn eq(&self, other: &$ucstring) -> bool {
                self == other.as_ucstr()
            }
        }

        impl PartialEq<$ucstring> for &$ucstr {
            #[inline]
            fn eq(&self, other: &$ucstring) -> bool {
                self == other.as_ucstr()
            }
        }

        impl PartialEq<$ucstring> for &$ustr {
            #[inline]
            fn eq(&self, other: &$ucstring) -> bool {
                self == other.as_ucstr()
            }
        }

        impl PartialOrd<$ustr> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &$ustr) -> Option<cmp::Ordering> {
                self.as_ucstr().partial_cmp(other)
            }
        }

        impl PartialOrd<$ucstr> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &$ucstr) -> Option<cmp::Ordering> {
                self.as_ucstr().partial_cmp(other)
            }
        }

        impl<'a> PartialOrd<&'a $ustr> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &&'a $ustr) -> Option<cmp::Ordering> {
                self.as_ucstr().partial_cmp(*other)
            }
        }

        impl<'a> PartialOrd<&'a $ucstr> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &&'a $ucstr) -> Option<cmp::Ordering> {
                self.as_ucstr().partial_cmp(*other)
            }
        }

        impl<'a> PartialOrd<Cow<'a, $ustr>> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &Cow<'a, $ustr>) -> Option<cmp::Ordering> {
                self.as_ucstr().partial_cmp(other.as_ref())
            }
        }

        impl<'a> PartialOrd<Cow<'a, $ucstr>> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &Cow<'a, $ucstr>) -> Option<cmp::Ordering> {
                self.as_ucstr().partial_cmp(other.as_ref())
            }
        }

        impl PartialOrd<$ustring> for $ucstring {
            #[inline]
            fn partial_cmp(&self, other: &$ustring) -> Option<cmp::Ordering> {
                self.as_ustr().partial_cmp(other.as_ustr())
            }
        }

        impl ToOwned for $ucstr {
            type Owned = $ucstring;

            #[inline]
            fn to_owned(&self) -> $ucstring {
                self.to_ucstring()
            }
        }
    };
}

ucstring_common_impl! {
    /// An owned, mutable C-style 16-bit wide string for FFI that is nul-aware and nul-terminated.
    ///
    /// The string slice of a [`U16CString`] is [`U16CStr`].
    ///
    /// [`U16CString`] strings do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-16 data, they may be used for
    /// any wide encoded string.
    ///
    /// # Nul termination
    ///
    /// [`U16CString`] is aware of nul (`0`) values. Unless unchecked conversions are used, all
    /// [`U16CString`] strings end with a nul-terminator in the underlying buffer and contain no
    /// internal nul values. These strings are intended to be used with FFI functions that require
    /// nul-terminated strings.
    ///
    /// Because of the nul termination requirement, multiple classes methods for provided for
    /// construction a [`U16CString`] under various scenarios. By default, methods such as
    /// [`from_ptr`][Self::from_ptr] and [`from_vec`][Self::from_vec] return an error if it contains
    /// any interior nul values before the terminator. For these methods, the input does not need to
    /// contain the terminating nul; it is added if it is does not exist.
    ///
    /// `_truncate` methods on the other hand, such as
    /// [`from_ptr_truncate`][Self::from_ptr_truncate] and
    /// [`from_vec_truncate`][Self::from_vec_truncate], construct a string that terminates with
    /// the first nul value encountered in the string, and do not return an error. They
    /// automatically ensure the string is terminated in a nul value even if it was not originally.
    ///
    /// Finally, unsafe `_unchecked` variants of these methods, such as
    /// [`from_ptr_unchecked`][Self::from_ptr_unchecked] and
    /// [`from_vec_unchecked`][Self::from_vec_unchecked] allow bypassing any checks for nul
    /// values, when the input has already been ensured to no interior nul values. Again, any
    /// missing nul terminator is automatically added if necessary.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U16CString`] outside of FFI is with the
    /// [`u16cstr!`][crate::u16cstr] macro to convert string literals into nul-terminated UTF-16
    /// strings at compile time:
    ///
    /// ```
    /// use widestring::{u16cstr, U16CString};
    /// let hello = U16CString::from(u16cstr!("Hello, world!"));
    /// ```
    ///
    /// You can also convert any [`u16`] slice or vector directly:
    ///
    /// ```
    /// use widestring::{u16cstr, U16CString};
    ///
    /// let sparkle_heart = vec![0xd83d, 0xdc96];
    /// let sparkle_heart = U16CString::from_vec(sparkle_heart).unwrap();
    /// // The string will add the missing nul terminator
    ///
    /// assert_eq!(u16cstr!("💖"), sparkle_heart);
    ///
    /// // This unpaired UTf-16 surrogate is invalid UTF-16, but is perfectly valid in U16CString
    /// let malformed_utf16 = vec![0xd83d, 0x0];
    /// let s = U16CString::from_vec(malformed_utf16).unwrap();
    ///
    /// assert_eq!(s.len(), 1); // Note the terminating nul is not counted in the length
    /// ```
    ///
    /// When working with a FFI, it is useful to create a [`U16CString`] from a pointer:
    ///
    /// ```
    /// use widestring::{u16cstr, U16CString};
    ///
    /// let sparkle_heart = [0xd83d, 0xdc96, 0x0];
    /// let s = unsafe {
    ///     // Note the string and pointer length does not include the nul terminator
    ///     U16CString::from_ptr(sparkle_heart.as_ptr(), sparkle_heart.len() - 1).unwrap()
    /// };
    /// assert_eq!(u16cstr!("💖"), s);
    ///
    /// // Alternatively, if the length of the pointer is unknown but definitely terminates in nul,
    /// // a C-style string version can be used
    /// let s = unsafe { U16CString::from_ptr_str(sparkle_heart.as_ptr()) };
    ///
    /// assert_eq!(u16cstr!("💖"), s);
    /// ```
    struct U16CString([u16]);

    type UCStr = U16CStr;
    type UString = U16String;
    type UStr = U16Str;

    /// Constructs a wide C string from a container of wide character data.
    ///
    /// This method will consume the provided data and use the underlying elements to
    /// construct a new string. The data will be scanned for invalid interior nul values.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value that is not the
    /// terminating nul.
    /// The returned error will contain the original [`Vec`] as well as the position of the
    /// nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let v = vec![84u16, 104u16, 101u16]; // 'T' 'h' 'e'
    /// # let cloned = v.clone();
    /// // Create a wide string from the vector
    /// let wcstr = U16CString::from_vec(v).unwrap();
    /// # assert_eq!(wcstr.into_vec(), cloned);
    /// ```
    ///
    /// Empty vectors are valid and will return an empty string with a nul terminator:
    ///
    /// ```
    /// use widestring::U16CString;
    /// let wcstr = U16CString::from_vec(vec![]).unwrap();
    /// assert_eq!(wcstr, U16CString::default());
    /// ```
    ///
    /// The following example demonstrates errors from nul values in a vector.
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let v = vec![84u16, 0u16, 104u16, 101u16]; // 'T' NUL 'h' 'e'
    /// // Create a wide string from the vector
    /// let res = U16CString::from_vec(v);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 1);
    /// ```
    fn from_vec() -> {}

    /// Constructs a wide C string from a container of wide character data, truncating at
    /// the first nul terminator.
    ///
    /// The string will be truncated at the first nul value in the data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let v = vec![84u16, 104u16, 101u16, 0u16]; // 'T' 'h' 'e' NUL
    /// # let cloned = v[..3].to_owned();
    /// // Create a wide string from the vector
    /// let wcstr = U16CString::from_vec_truncate(v);
    /// # assert_eq!(wcstr.into_vec(), cloned);
    /// ```
    fn from_vec_truncate() -> {}

    /// Converts this wide C string into a boxed wide C string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U16CString, U16CStr};
    ///
    /// let mut v = vec![102u16, 111u16, 111u16]; // "foo"
    /// let c_string = U16CString::from_vec(v.clone()).unwrap();
    /// let boxed = c_string.into_boxed_ucstr();
    /// v.push(0);
    /// assert_eq!(&*boxed, U16CStr::from_slice(&v).unwrap());
    /// ```
    fn into_boxed_ucstr() -> {}
}
ucstring_common_impl! {
    /// An owned, mutable C-style 32-bit wide string for FFI that is nul-aware and nul-terminated.
    ///
    /// The string slice of a [`U32CString`] is [`U32CStr`].
    ///
    /// [`U32CString`] strings do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-32 data, they may be used for
    /// any wide encoded string.
    ///
    /// # Nul termination
    ///
    /// [`U32CString`] is aware of nul (`0`) values. Unless unchecked conversions are used, all
    /// [`U32CString`] strings end with a nul-terminator in the underlying buffer and contain no
    /// internal nul values. These strings are intended to be used with FFI functions that require
    /// nul-terminated strings.
    ///
    /// Because of the nul termination requirement, multiple classes methods for provided for
    /// construction a [`U32CString`] under various scenarios. By default, methods such as
    /// [`from_ptr`][Self::from_ptr] and [`from_vec`][Self::from_vec] return an error if it contains
    /// any interior nul values before the terminator. For these methods, the input does not need to
    /// contain the terminating nul; it is added if it is does not exist.
    ///
    /// `_truncate` methods on the other hand, such as
    /// [`from_ptr_truncate`][Self::from_ptr_truncate] and
    /// [`from_vec_truncate`][Self::from_vec_truncate], construct a string that terminates with
    /// the first nul value encountered in the string, and do not return an error. They
    /// automatically ensure the string is terminated in a nul value even if it was not originally.
    ///
    /// Finally, unsafe `_unchecked` variants of these methods, such as
    /// [`from_ptr_unchecked`][Self::from_ptr_unchecked] and
    /// [`from_vec_unchecked`][Self::from_vec_unchecked] allow bypassing any checks for nul
    /// values, when the input has already been ensured to no interior nul values. Again, any
    /// missing nul terminator is automatically added if necessary.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U32CString`] outside of FFI is with the
    /// [`u32cstr!`][crate::u32cstr] macro to convert string literals into nul-terminated UTF-32
    /// strings at compile time:
    ///
    /// ```
    /// use widestring::{u32cstr, U32CString};
    /// let hello = U32CString::from(u32cstr!("Hello, world!"));
    /// ```
    ///
    /// You can also convert any [`u32`] slice or vector directly:
    ///
    /// ```
    /// use widestring::{u32cstr, U32CString};
    ///
    /// let sparkle_heart = vec![0x1f496];
    /// let sparkle_heart = U32CString::from_vec(sparkle_heart).unwrap();
    /// // The string will add the missing nul terminator
    ///
    /// assert_eq!(u32cstr!("💖"), sparkle_heart);
    ///
    /// // This UTf-16 surrogate is invalid UTF-32, but is perfectly valid in U32CString
    /// let malformed_utf32 = vec![0xd83d, 0x0];
    /// let s = U32CString::from_vec(malformed_utf32).unwrap();
    ///
    /// assert_eq!(s.len(), 1); // Note the terminating nul is not counted in the length
    /// ```
    ///
    /// When working with a FFI, it is useful to create a [`U32CString`] from a pointer:
    ///
    /// ```
    /// use widestring::{u32cstr, U32CString};
    ///
    /// let sparkle_heart = [0x1f496, 0x0];
    /// let s = unsafe {
    ///     // Note the string and pointer length does not include the nul terminator
    ///     U32CString::from_ptr(sparkle_heart.as_ptr(), sparkle_heart.len() - 1).unwrap()
    /// };
    /// assert_eq!(u32cstr!("💖"), s);
    ///
    /// // Alternatively, if the length of the pointer is unknown but definitely terminates in nul,
    /// // a C-style string version can be used
    /// let s = unsafe { U32CString::from_ptr_str(sparkle_heart.as_ptr()) };
    ///
    /// assert_eq!(u32cstr!("💖"), s);
    /// ```
    struct U32CString([u32]);

    type UCStr = U32CStr;
    type UString = U32String;
    type UStr = U32Str;

    /// Constructs a wide C string from a container of wide character data.
    ///
    /// This method will consume the provided data and use the underlying elements to
    /// construct a new string. The data will be scanned for invalid interior nul values.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value that is not the
    /// terminating nul.
    /// The returned error will contain the original [`Vec`] as well as the position of the
    /// nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let v = vec![84u32, 104u32, 101u32]; // 'T' 'h' 'e'
    /// # let cloned = v.clone();
    /// // Create a wide string from the vector
    /// let wcstr = U32CString::from_vec(v).unwrap();
    /// # assert_eq!(wcstr.into_vec(), cloned);
    /// ```
    ///
    /// Empty vectors are valid and will return an empty string with a nul terminator:
    ///
    /// ```
    /// use widestring::U32CString;
    /// let wcstr = U32CString::from_vec(vec![]).unwrap();
    /// assert_eq!(wcstr, U32CString::default());
    /// ```
    ///
    /// The following example demonstrates errors from nul values in a vector.
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let v = vec![84u32, 0u32, 104u32, 101u32]; // 'T' NUL 'h' 'e'
    /// // Create a wide string from the vector
    /// let res = U32CString::from_vec(v);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 1);
    /// ```
    fn from_vec() -> {}

    /// Constructs a wide C string from a container of wide character data, truncating at
    /// the first nul terminator.
    ///
    /// The string will be truncated at the first nul value in the data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let v = vec![84u32, 104u32, 101u32, 0u32]; // 'T' 'h' 'e' NUL
    /// # let cloned = v[..3].to_owned();
    /// // Create a wide string from the vector
    /// let wcstr = U32CString::from_vec_truncate(v);
    /// # assert_eq!(wcstr.into_vec(), cloned);
    /// ```
    fn from_vec_truncate() -> {}

    /// Converts this wide C string into a boxed wide C string slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::{U32CString, U32CStr};
    ///
    /// let mut v = vec![102u32, 111u32, 111u32]; // "foo"
    /// let c_string = U32CString::from_vec(v.clone()).unwrap();
    /// let boxed = c_string.into_boxed_ucstr();
    /// v.push(0);
    /// assert_eq!(&*boxed, U32CStr::from_slice(&v).unwrap());
    /// ```
    fn into_boxed_ucstr() -> {}
}

impl U16CString {
    /// Constructs a [`U16CString`] copy from a [`str`], encoding it as UTF-16.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U16CString`] will also be valid UTF-16.
    ///
    /// The string will be scanned for nul values, which are invalid anywhere except the final
    /// character.
    ///
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value anywhere except the
    /// final position.
    /// The returned error will contain a [`Vec<u16>`] as well as the position of the nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = U16CString::from_str(s).unwrap();
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    ///
    /// The following example demonstrates errors from nul values in a string.
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let res = U16CString::from_str(s);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 2);
    /// ```
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn from_str(s: impl AsRef<str>) -> Result<Self, ContainsNul<u16>> {
        let v: Vec<u16> = s.as_ref().encode_utf16().collect();
        Self::from_vec(v)
    }

    /// Constructs a [`U16CString`] copy from a [`str`], encoding it as UTF-16, without checking for
    /// interior nul values.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U16CString`] will also be valid UTF-16.
    ///
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Safety
    ///
    /// This method is equivalent to [`from_str`][Self::from_str] except that no runtime assertion
    /// is made that `s` contains no interior nul values. Providing a string with nul values that
    /// are not the last character will result in an invalid [`U16CString`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = unsafe { U16CString::from_str_unchecked(s) };
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_str_unchecked(s: impl AsRef<str>) -> Self {
        let v: Vec<u16> = s.as_ref().encode_utf16().collect();
        Self::from_vec_unchecked(v)
    }

    /// Constructs a [`U16CString`] copy from a [`str`], encoding it as UTF-16, truncating at the
    /// first nul terminator.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U16CString`] will also be valid UTF-16.
    ///
    /// The string will be truncated at the first nul value in the string.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let wcstr = U16CString::from_str_truncate(s);
    /// assert_eq!(wcstr.to_string_lossy(), "My");
    /// ```
    #[inline]
    #[must_use]
    pub fn from_str_truncate(s: impl AsRef<str>) -> Self {
        let v: Vec<u16> = s.as_ref().encode_utf16().collect();
        Self::from_vec_truncate(v)
    }

    /// Constructs a [`U16CString`] copy from an [`OsStr`][std::ffi::OsStr].
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U16CString`] will be valid UTF-16.
    ///
    /// The string will be scanned for nul values, which are invalid anywhere except the final
    /// character.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms (such as
    /// windows) no changes to the string will be made.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value anywhere except the
    /// last character.
    /// The returned error will contain a [`Vec<u16>`] as well as the position of the nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = U16CString::from_os_str(s).unwrap();
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    ///
    /// The following example demonstrates errors from nul values in the string.
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let res = U16CString::from_os_str(s);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 2);
    /// ```
    #[inline]
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn from_os_str(s: impl AsRef<std::ffi::OsStr>) -> Result<Self, ContainsNul<u16>> {
        let v = crate::platform::os_to_wide(s.as_ref());
        Self::from_vec(v)
    }

    /// Constructs a [`U16CString`] copy from an [`OsStr`][std::ffi::OsStr], without checking for nul
    /// values.
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U16CString`] will be valid UTF-16.
    ///
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms (such as
    /// windows) no changes to the string will be made.
    ///
    /// # Safety
    ///
    /// This method is equivalent to [`from_os_str`][Self::from_os_str] except that no runtime
    /// assertion is made that `s` contains no interior nul values. Providing a string with nul
    /// values anywhere but the last character will result in an invalid [`U16CString`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = unsafe { U16CString::from_os_str_unchecked(s) };
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[must_use]
    pub unsafe fn from_os_str_unchecked(s: impl AsRef<std::ffi::OsStr>) -> Self {
        let v = crate::platform::os_to_wide(s.as_ref());
        Self::from_vec_unchecked(v)
    }

    /// Constructs a [`U16CString`] copy from an [`OsStr`][std::ffi::OsStr], truncating at the first
    /// nul terminator.
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U16CString`] will be valid UTF-16.
    ///
    /// The string will be truncated at the first nul value in the string.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms (such as
    /// windows) no changes to the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let wcstr = U16CString::from_os_str_truncate(s);
    /// assert_eq!(wcstr.to_string_lossy(), "My");
    /// ```
    #[inline]
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[must_use]
    pub fn from_os_str_truncate(s: impl AsRef<std::ffi::OsStr>) -> Self {
        let v = crate::platform::os_to_wide(s.as_ref());
        Self::from_vec_truncate(v)
    }
}

impl U32CString {
    /// Constructs a [`U32CString`] from a container of character data, checking for invalid nul
    /// values.
    ///
    /// This method will consume the provided data and use the underlying elements to construct a
    /// new string. The data will be scanned for invalid nul values anywhere except the last
    /// character.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value anywhere except the
    /// last character.
    /// The returned error will contain the [`Vec<u32>`] as well as the position of the nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let v: Vec<char> = "Test".chars().collect();
    /// # let cloned: Vec<u32> = v.iter().map(|&c| c as u32).collect();
    /// // Create a wide string from the vector
    /// let wcstr = U32CString::from_chars(v).unwrap();
    /// # assert_eq!(wcstr.into_vec(), cloned);
    /// ```
    ///
    /// The following example demonstrates errors from nul values in a vector.
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let v: Vec<char> = "T\u{0}est".chars().collect();
    /// // Create a wide string from the vector
    /// let res = U32CString::from_chars(v);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 1);
    /// ```
    pub fn from_chars(v: impl Into<Vec<char>>) -> Result<Self, ContainsNul<u32>> {
        let mut chars = v.into();
        let v: Vec<u32> = unsafe {
            let ptr = chars.as_mut_ptr() as *mut u32;
            let len = chars.len();
            let cap = chars.capacity();
            mem::forget(chars);
            Vec::from_raw_parts(ptr, len, cap)
        };
        Self::from_vec(v)
    }

    /// Constructs a [`U32CString`] from a container of character data, truncating at the first nul
    /// value.
    ///
    /// This method will consume the provided data and use the underlying elements to construct a
    /// new string. The string will be truncated at the first nul value in the string.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let v: Vec<char> = "Test\u{0}".chars().collect();
    /// # let cloned: Vec<u32> = v[..4].iter().map(|&c| c as u32).collect();
    /// // Create a wide string from the vector
    /// let wcstr = U32CString::from_chars_truncate(v);
    /// # assert_eq!(wcstr.into_vec(), cloned);
    /// ```
    #[must_use]
    pub fn from_chars_truncate(v: impl Into<Vec<char>>) -> Self {
        let mut chars = v.into();
        let v: Vec<u32> = unsafe {
            let ptr = chars.as_mut_ptr() as *mut u32;
            let len = chars.len();
            let cap = chars.capacity();
            mem::forget(chars);
            Vec::from_raw_parts(ptr, len, cap)
        };
        Self::from_vec_truncate(v)
    }

    /// Constructs a [`U32CString`] from character data without checking for nul values.
    ///
    /// A terminating nul value will be appended if the vector does not already have a terminating
    /// nul.
    ///
    /// # Safety
    ///
    /// This method is equivalent to [`from_chars`][Self::from_chars] except that no runtime
    /// assertion is made that `v` contains no interior nul values. Providing a vector with nul
    /// values anywhere but the last character will result in an invalid [`U32CString`].
    #[must_use]
    pub unsafe fn from_chars_unchecked(v: impl Into<Vec<char>>) -> Self {
        let mut chars = v.into();
        let v: Vec<u32> = {
            let ptr = chars.as_mut_ptr() as *mut u32;
            let len = chars.len();
            let cap = chars.capacity();
            mem::forget(chars);
            Vec::from_raw_parts(ptr, len, cap)
        };
        Self::from_vec_unchecked(v)
    }

    /// Constructs a [`U32CString`] copy from a [`str`], encoding it as UTF-32 and checking for
    /// invalid interior nul values.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U32CString`] will also be valid UTF-32.
    ///
    /// The string will be scanned for nul values, which are invalid anywhere except the last
    /// character.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value anywhere except the
    /// last character.
    /// The returned error will contain a [`Vec<u32>`] as well as the position of the nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = U32CString::from_str(s).unwrap();
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    ///
    /// The following example demonstrates errors from nul values in a string.
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let res = U32CString::from_str(s);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 2);
    /// ```
    #[allow(clippy::should_implement_trait)]
    #[inline]
    pub fn from_str(s: impl AsRef<str>) -> Result<Self, ContainsNul<u32>> {
        let v: Vec<char> = s.as_ref().chars().collect();
        Self::from_chars(v)
    }

    /// Constructs a [`U32CString`] copy from a [`str`], encoding it as UTF-32, without checking for
    /// nul values.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U32CString`] will also be valid UTF-32.
    ///
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Safety
    ///
    /// This method is equivalent to [`from_str`][Self::from_str] except that no runtime assertion
    /// is made that `s` contains invalid nul values. Providing a string with nul values anywhere
    /// except the last character will result in an invalid [`U32CString`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = unsafe { U32CString::from_str_unchecked(s) };
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    #[inline]
    #[must_use]
    pub unsafe fn from_str_unchecked(s: impl AsRef<str>) -> Self {
        let v: Vec<char> = s.as_ref().chars().collect();
        Self::from_chars_unchecked(v)
    }

    /// Constructs a [`U32CString`] copy from a [`str`], encoding it as UTF-32, truncating at the
    /// first nul terminator.
    ///
    /// This makes a string copy of the [`str`]. Since [`str`] will always be valid UTF-8, the
    /// resulting [`U32CString`] will also be valid UTF-32.
    ///
    /// The string will be truncated at the first nul value in the string.
    /// The resulting string will always be nul-terminated even if the original string is not.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let wcstr = U32CString::from_str_truncate(s);
    /// assert_eq!(wcstr.to_string_lossy(), "My");
    /// ```
    #[inline]
    #[must_use]
    pub fn from_str_truncate(s: impl AsRef<str>) -> Self {
        let v: Vec<char> = s.as_ref().chars().collect();
        Self::from_chars_truncate(v)
    }

    /// Constructs a new wide C string copied from a nul-terminated [`char`] string pointer.
    ///
    /// This will scan for nul values beginning with `p`. The first nul value will be used as the
    /// nul terminator for the string, similar to how libc string functions such as `strlen` work.
    ///
    /// If you wish to avoid copying the string pointer, use [`U32CStr::from_char_ptr_str`] instead.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid or has a
    /// nul terminator, and the function could scan past the underlying buffer.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts].
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
    pub unsafe fn from_char_ptr_str(p: *const char) -> Self {
        Self::from_ptr_str(p as *const u32)
    }

    /// Constructs a wide C string copied from a [`char`] pointer and a length, checking for invalid
    /// interior nul values.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes, and does
    /// **not** include the nul terminator of the string. If `len` is `0`, `p` is allowed to be a
    /// null pointer.
    ///
    /// The resulting string will always be nul-terminated even if the pointer data is not.
    ///
    /// # Errors
    ///
    /// This will scan the pointer string for an interior nul value and error if one is found. To
    /// avoid scanning for interior nuls, [`from_ptr_unchecked`][Self::from_ptr_unchecked] may be
    /// used instead.
    /// The returned error will contain a [`Vec`] as well as the position of the nul value.
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
    pub unsafe fn from_char_ptr(p: *const char, len: usize) -> Result<Self, ContainsNul<u32>> {
        Self::from_ptr(p as *const u32, len)
    }

    /// Constructs a wide C string copied from a [`char`] pointer and a length, truncating at the
    /// first nul terminator.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes. This will scan
    /// for nul values beginning with `p` until offset `len`. The first nul value will be used as
    /// the nul terminator for the string, ignoring any remaining values left before `len`. If no
    /// nul value is found, the whole string of length `len` is used, and a new nul-terminator
    /// will be added to the resulting string. If `len` is `0`, `p` is allowed to be a null pointer.
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
    pub unsafe fn from_char_ptr_truncate(p: *const char, len: usize) -> Self {
        Self::from_ptr_truncate(p as *const u32, len)
    }

    /// Constructs a wide C string copied from a [`char`] pointer and a length without checking for
    /// any nul values.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes, and does
    /// **not** include the nul terminator of the string. If `len` is `0`, `p` is allowed to be a
    /// null pointer.
    ///
    /// The resulting string will always be nul-terminated even if the pointer data is not.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len`
    /// elements.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts].
    ///
    /// The interior values of the pointer are not scanned for nul. Any interior nul values or
    /// will result in an invalid C string.
    ///
    /// # Panics
    ///
    /// Panics if `len` is greater than 0 but `p` is a null pointer.
    #[must_use]
    pub unsafe fn from_char_ptr_unchecked(p: *const char, len: usize) -> Self {
        Self::from_ptr_unchecked(p as *const u32, len)
    }

    /// Constructs a [`U32CString`] copy from an [`OsStr`][std::ffi::OsStr], checking for invalid
    /// nul values.
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U32CString`] will be valid UTF-32.
    ///
    /// The string will be scanned for nul values, which are invlaid anywhere except the last
    /// character.
    /// The resulting string will always be nul-terminated even if the string is not.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms no changes to
    /// the string will be made.
    ///
    /// # Errors
    ///
    /// This function will return an error if the data contains a nul value anywhere except the
    /// last character.
    /// The returned error will contain a [`Vec<u32>`] as well as the position of the nul value.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = U32CString::from_os_str(s).unwrap();
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    ///
    /// The following example demonstrates errors from nul values in a string.
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let res = U32CString::from_os_str(s);
    /// assert!(res.is_err());
    /// assert_eq!(res.err().unwrap().nul_position(), 2);
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    pub fn from_os_str(s: impl AsRef<std::ffi::OsStr>) -> Result<Self, ContainsNul<u32>> {
        let v: Vec<char> = s.as_ref().to_string_lossy().chars().collect();
        Self::from_chars(v)
    }

    /// Constructs a [`U32CString`] copy from an [`OsStr`][std::ffi::OsStr], without checking for
    /// nul values.
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U32CString`] will be valid UTF-32.
    ///
    /// The resulting string will always be nul-terminated even if the string is not.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms no changes to
    /// the string will be made.
    ///
    /// # Safety
    ///
    /// This method is equivalent to [`from_os_str`][Self::from_os_str] except that no runtime
    /// assertion is made that `s` contains invalid nul values. Providing a string with nul values
    /// anywhere except the last character will result in an invalid [`U32CString`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wcstr = unsafe { U32CString::from_os_str_unchecked(s) };
    /// # assert_eq!(wcstr.to_string_lossy(), s);
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    #[must_use]
    pub unsafe fn from_os_str_unchecked(s: impl AsRef<std::ffi::OsStr>) -> Self {
        let v: Vec<char> = s.as_ref().to_string_lossy().chars().collect();
        Self::from_chars_unchecked(v)
    }

    /// Constructs a [`U32CString`] copy from an [`OsStr`][std::ffi::OsStr], truncating at the first
    /// nul terminator.
    ///
    /// This makes a string copy of the [`OsStr`][std::ffi::OsStr]. Since [`OsStr`][std::ffi::OsStr]
    /// makes no guarantees that it is valid data, there is no guarantee that the resulting
    /// [`U32CString`] will be valid UTF-32.
    ///
    /// The string will be truncated at the first nul value in the string.
    /// The resulting string will always be nul-terminated even if the string is not.
    ///
    /// Note that the encoding of [`OsStr`][std::ffi::OsStr] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms no changes to
    /// the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "My\u{0}String";
    /// // Create a wide string from the string
    /// let wcstr = U32CString::from_os_str_truncate(s);
    /// assert_eq!(wcstr.to_string_lossy(), "My");
    /// ```
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[inline]
    #[must_use]
    pub fn from_os_str_truncate(s: impl AsRef<std::ffi::OsStr>) -> Self {
        let v: Vec<char> = s.as_ref().to_string_lossy().chars().collect();
        Self::from_chars_truncate(v)
    }
}

impl core::fmt::Debug for U16CString {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u16(self.as_slice_with_nul(), f)
    }
}

impl core::fmt::Debug for U32CString {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u32(self.as_slice_with_nul(), f)
    }
}

/// Alias for [`U16CString`] or [`U32CString`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(not(windows))]
pub type WideCString = U32CString;

/// Alias for [`U16CString`] or [`U32CString`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(windows)]
pub type WideCString = U16CString;
