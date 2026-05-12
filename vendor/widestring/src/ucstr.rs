//! C-style wide string slices.
//!
//! This module contains wide C string slices and related types.

use crate::{
    error::{ContainsNul, MissingNulTerminator, NulError},
    U16Str, U32Str,
};
#[cfg(feature = "alloc")]
#[allow(unused_imports)]
use alloc::{borrow::ToOwned, boxed::Box, string::String};
use core::{
    fmt::Write,
    ops::{Index, Range},
    slice::{self, SliceIndex},
};

#[doc(inline)]
pub use crate::ustr::{
    CharIndicesLossyUtf16, CharIndicesLossyUtf32, CharIndicesUtf16, CharIndicesUtf32,
    CharsLossyUtf16, CharsLossyUtf32, CharsUtf16, CharsUtf32,
};

macro_rules! ucstr_common_impl {
    {
        $(#[$ucstr_meta:meta])*
        struct $ucstr:ident([$uchar:ty]);
        type UCString = $ucstring:ident;
        type UStr = $ustr:ident;
        type UString = $ustring:ident;
        $(#[$to_ustring_meta:meta])*
        fn to_ustring() -> {}
        $(#[$into_ucstring_meta:meta])*
        fn into_ucstring() -> {}
        $(#[$display_meta:meta])*
        fn display() -> {}
    } => {
        $(#[$ucstr_meta])*
        #[allow(clippy::derive_hash_xor_eq)]
        #[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $ucstr {
            inner: [$uchar],
        }

        impl $ucstr {
            /// The nul terminator character value.
            pub const NUL_TERMINATOR: $uchar = 0;

            /// Coerces a value into a wide C string slice.
            #[inline]
            #[must_use]
            pub fn new<S: AsRef<$ucstr> + ?Sized>(s: &S) -> &Self {
                s.as_ref()
            }

            /// Constructs a wide C string slice from a nul-terminated string pointer.
            ///
            /// This will scan for nul values beginning with `p`. The first nul value will be used
            /// as the nul terminator for the string, similar to how libc string functions such as
            /// `strlen` work.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid or
            /// has a nul terminator, and the function could scan past the underlying buffer.
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
            #[must_use]
            pub unsafe fn from_ptr_str<'a>(p: *const $uchar) -> &'a Self {
                assert!(!p.is_null());
                let mut i = 0;
                while *p.add(i) != Self::NUL_TERMINATOR {
                    i += 1;
                }
                Self::from_ptr_unchecked(p, i)
            }

            /// Constructs a mutable wide C string slice from a mutable nul-terminated string
            /// pointer.
            ///
            /// This will scan for nul values beginning with `p`. The first nul value will be used
            /// as the nul terminator for the string, similar to how libc string functions such as
            /// `strlen` work.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid or
            /// has a nul terminator, and the function could scan past the underlying buffer.
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
            #[must_use]
            pub unsafe fn from_ptr_str_mut<'a>(p: *mut $uchar) -> &'a mut Self {
                assert!(!p.is_null());
                let mut i = 0;
                while *p.add(i) != Self::NUL_TERMINATOR {
                    i += 1;
                }
                Self::from_ptr_unchecked_mut(p, i)
            }

            /// Constructs a wide C string slice from a pointer and a length.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes, and does
            /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and
            /// means that `p` is a pointer directly to the nul terminator of the string.
            ///
            /// # Errors
            ///
            /// This will scan the pointer string for an interior nul value and error if one is
            /// found before the nul terminator at `len` offset. To avoid scanning for interior
            /// nuls, [`from_ptr_unchecked`][Self::from_ptr_unchecked] may be used instead.
            ///
            /// An error is returned if the value at `len` offset is not a nul terminator.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len + 1` elements.
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
            pub unsafe fn from_ptr<'a>(
                p: *const $uchar,
                len: usize,
            ) -> Result<&'a Self, NulError<$uchar>> {
                assert!(!p.is_null());
                if *p.add(len) != Self::NUL_TERMINATOR {
                    return Err(MissingNulTerminator::new().into());
                }
                for i in 0..len {
                    if *p.add(i) == Self::NUL_TERMINATOR {
                        return Err(ContainsNul::empty(i).into());
                    }
                }
                Ok(Self::from_ptr_unchecked(p, len))
            }

            /// Constructs a mutable wide C string slice from a mutable pointer and a length.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes, and does
            /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and
            /// means that `p` is a pointer directly to the nul terminator of the string.
            ///
            /// # Errors
            ///
            /// This will scan the pointer string for an interior nul value and error if one is
            /// found before the nul terminator at `len` offset. To avoid scanning for interior
            /// nuls, [`from_ptr_unchecked_mut`][Self::from_ptr_unchecked_mut] may be used instead.
            ///
            /// An error is returned if the value at `len` offset is not a nul terminator.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len + 1` elements.
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
            pub unsafe fn from_ptr_mut<'a>(
                p: *mut $uchar,
                len: usize,
            ) -> Result<&'a mut Self, NulError<$uchar>> {
                assert!(!p.is_null());
                if *p.add(len) != Self::NUL_TERMINATOR {
                    return Err(MissingNulTerminator::new().into());
                }
                for i in 0..len {
                    if *p.add(i) == Self::NUL_TERMINATOR {
                        return Err(ContainsNul::empty(i).into());
                    }
                }
                Ok(Self::from_ptr_unchecked_mut(p, len))
            }

            /// Constructs a wide C string slice from a pointer and a length, truncating at the
            /// first nul terminator.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes. This will
            /// scan for nul values beginning with `p` until offset `len`. The first nul value will
            /// be used as the nul terminator for the string, ignoring any remaining values left
            /// before `len`.
            ///
            /// # Errors
            ///
            /// If no nul terminator is found after `len` + 1 elements, an error is returned.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid or
            /// has a nul terminator, and the function could scan past the underlying buffer.
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
            /// of a host value for thev string, or by explicit annotation.
            pub unsafe fn from_ptr_truncate<'a>(
                p: *const $uchar,
                len: usize,
            ) -> Result<&'a Self, MissingNulTerminator> {
                assert!(!p.is_null());
                for i in 0..=len {
                    if *p.add(i) == Self::NUL_TERMINATOR {
                        return Ok(Self::from_ptr_unchecked(p, i));
                    }
                }
                Err(MissingNulTerminator::new())
            }

            /// Constructs a mutable wide C string slice from a mutable pointer and a length,
            /// truncating at the first nul terminator.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes. This will
            /// scan for nul values beginning with `p` until offset `len`. The first nul value will
            /// be used as the nul terminator for the string, ignoring any remaining values left
            /// before `len`.
            ///
            /// # Errors
            ///
            /// If no nul terminator is found after `len` + 1 elements, an error is returned.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid or
            /// has a nul terminator, and the function could scan past the underlying buffer.
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
            pub unsafe fn from_ptr_truncate_mut<'a>(
                p: *mut $uchar,
                len: usize,
            ) -> Result<&'a mut Self, MissingNulTerminator> {
                assert!(!p.is_null());
                for i in 0..=len {
                    if *p.add(i) == Self::NUL_TERMINATOR {
                        return Ok(Self::from_ptr_unchecked_mut(p, i));
                    }
                }
                Err(MissingNulTerminator::new())
            }

            /// Constructs a wide C string slice from a pointer and a length without checking for
            /// any nul values.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes, and does
            /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and
            /// means that `p` is a pointer directly to the nul terminator of the string.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len + 1` elements, nor that it has a terminating nul value.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts]. In particular, the returned string reference *must not
            /// be mutated* for the duration of lifetime `'a`, except inside an
            /// [`UnsafeCell`][std::cell::UnsafeCell].
            ///
            /// The interior values of the pointer are not scanned for nul. Any interior nul values
            /// or a missing nul terminator at pointer offset `len` + 1 will result in an invalid
            /// string slice.
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
            #[must_use]
            pub unsafe fn from_ptr_unchecked<'a>(p: *const $uchar, len: usize) -> &'a Self {
                assert!(!p.is_null());
                let ptr: *const [$uchar] = slice::from_raw_parts(p, len + 1);
                &*(ptr as *const Self)
            }

            /// Constructs a mutable wide C string slice from a mutable pointer and a length without
            /// checking for any nul values.
            ///
            /// The `len` argument is the number of elements, **not** the number of bytes, and does
            /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and
            /// means that `p` is a pointer directly to the nul terminator of the string.
            ///
            /// # Safety
            ///
            /// This function is unsafe as there is no guarantee that the given pointer is valid for
            /// `len + 1` elements, nor that is has a terminating nul value.
            ///
            /// In addition, the data must meet the safety conditions of
            /// [std::slice::from_raw_parts_mut].
            ///
            /// The interior values of the pointer are not scanned for nul. Any interior nul values
            /// or a missing nul terminator at pointer offset `len` + 1 will result in an invalid
            /// string slice.
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
            #[must_use]
            pub unsafe fn from_ptr_unchecked_mut<'a>(p: *mut $uchar, len: usize) -> &'a mut Self {
                assert!(!p.is_null());
                let ptr: *mut [$uchar] = slice::from_raw_parts_mut(p, len + 1);
                &mut *(ptr as *mut Self)
            }

            /// Constructs a wide C string slice from a slice of values with a terminating nul,
            /// checking for invalid interior nul values.
            ///
            /// The slice must have at least one item, the nul terminator, even for an empty string.
            ///
            /// # Errors
            ///
            /// If there are nul values in the slice except for the last value, an error is
            /// returned.
            ///
            /// An error is also returned if the last value of the slice is not a nul terminator.
            pub fn from_slice(slice: &[$uchar]) -> Result<&Self, NulError<$uchar>> {
                if slice.last() != Some(&Self::NUL_TERMINATOR) {
                    return Err(MissingNulTerminator::new().into());
                }
                match slice[..slice.len() - 1]
                    .iter()
                    .position(|x| *x == Self::NUL_TERMINATOR)
                {
                    None => Ok(unsafe { Self::from_slice_unchecked(slice) }),
                    Some(i) => Err(ContainsNul::empty(i).into()),
                }
            }

            /// Constructs a mutable wide C string slice from a mutable slice of values with a
            /// terminating nul, checking for invalid interior nul values.
            ///
            /// The slice must have at least one item, the nul terminator, even for an empty string.
            ///
            /// # Errors
            ///
            /// If there are nul values in the slice except for the last value, an error is
            /// returned.
            ///
            /// An error is also returned if the last value of the slice is not a nul terminator.
            pub fn from_slice_mut(slice: &mut [$uchar]) -> Result<&mut Self, NulError<$uchar>> {
                if slice.last() != Some(&Self::NUL_TERMINATOR) {
                    return Err(MissingNulTerminator::new().into());
                }
                match slice[..slice.len() - 1]
                    .iter()
                    .position(|x| *x == Self::NUL_TERMINATOR)
                {
                    None => Ok(unsafe { Self::from_slice_unchecked_mut(slice) }),
                    Some(i) => Err(ContainsNul::empty(i).into()),
                }
            }

            /// Constructs a wide C string slice from a slice of values, truncating at the first nul
            /// terminator.
            ///
            /// The slice will be scanned for nul values. When a nul value is found, it is treated
            /// as the terminator for the string, and the string slice will be truncated to that
            /// nul.
            ///
            /// # Errors
            ///
            /// If there are no nul values in the slice, an error is returned.
            pub fn from_slice_truncate(slice: &[$uchar]) -> Result<&Self, MissingNulTerminator> {
                match slice.iter().position(|x| *x == Self::NUL_TERMINATOR) {
                    None => Err(MissingNulTerminator::new()),
                    Some(i) => Ok(unsafe { Self::from_slice_unchecked(&slice[..i + 1]) }),
                }
            }

            /// Constructs a mutable wide C string slice from a mutable slice of values, truncating
            /// at the first nul terminator.
            ///
            /// The slice will be scanned for nul values. When a nul value is found, it is treated
            /// as the terminator for the string, and the string slice will be truncated to that
            /// nul.
            ///
            /// # Errors
            ///
            /// If there are no nul values in the slice, an error is returned.
            pub fn from_slice_truncate_mut(
                slice: &mut [$uchar],
            ) -> Result<&mut Self, MissingNulTerminator> {
                match slice.iter().position(|x| *x == Self::NUL_TERMINATOR) {
                    None => Err(MissingNulTerminator::new()),
                    Some(i) => Ok(unsafe { Self::from_slice_unchecked_mut(&mut slice[..i + 1]) }),
                }
            }

            /// Constructs a wide C string slice from a slice of values without checking for a
            /// terminating or interior nul values.
            ///
            /// # Safety
            ///
            /// This function is unsafe because it can lead to invalid string slice values when the
            /// slice is missing a terminating nul value or there are non-terminating interior nul
            /// values in the slice. In particular, an empty slice will result in an invalid
            /// string slice.
            #[must_use]
            pub const unsafe fn from_slice_unchecked(slice: &[$uchar]) -> &Self {
                let ptr: *const [$uchar] = slice;
                &*(ptr as *const Self)
            }

            /// Constructs a mutable wide C string slice from a mutable slice of values without
            /// checking for a terminating or interior nul values.
            ///
            /// # Safety
            ///
            /// This function is unsafe because it can lead to invalid string slice values when the
            /// slice is missing a terminating nul value or there are non-terminating interior nul
            /// values in the slice. In particular, an empty slice will result in an invalid
            /// string slice.
            #[must_use]
            pub unsafe fn from_slice_unchecked_mut(slice: &mut [$uchar]) -> &mut Self {
                let ptr: *mut [$uchar] = slice;
                &mut *(ptr as *mut Self)
            }

            /// Copies the string reference to a new owned wide C string.
            #[inline]
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn to_ucstring(&self) -> crate::$ucstring {
                unsafe { crate::$ucstring::from_vec_unchecked(self.inner.to_owned()) }
            }

            $(#[$to_ustring_meta])*
            #[inline]
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn to_ustring(&self) -> crate::$ustring {
                crate::$ustring::from_vec(self.as_slice())
            }

            /// Converts to a slice of the underlying elements.
            ///
            /// The slice will **not** include the nul terminator.
            #[inline]
            #[must_use]
            pub fn as_slice(&self) -> &[$uchar] {
                &self.inner[..self.len()]
            }

            /// Converts to a mutable slice of the underlying elements.
            ///
            /// The slice will **not** include the nul terminator.
            ///
            /// # Safety
            ///
            /// This method is unsafe because you can violate the invariants of this type when
            /// mutating the slice (i.e. by adding interior nul values).
            #[inline]
            #[must_use]
            pub unsafe fn as_mut_slice(&mut self) -> &mut [$uchar] {
                let len = self.len();
                &mut self.inner[..len]
            }

            /// Converts to a slice of the underlying elements, including the nul terminator.
            #[inline]
            #[must_use]
            pub const fn as_slice_with_nul(&self) -> &[$uchar] {
                &self.inner
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

            /// Returns a mutable raw pointer to the string.
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

            /// Returns the length of the string as number of elements (**not** number of bytes)
            /// **not** including nul terminator.
            #[inline]
            #[must_use]
            pub const fn len(&self) -> usize {
                self.inner.len() - 1
            }

            /// Returns whether this string contains no data (i.e. is only the nul terminator).
            #[inline]
            #[must_use]
            pub const fn is_empty(&self) -> bool {
                self.len() == 0
            }

            $(#[$into_ucstring_meta])*
            #[cfg(feature = "alloc")]
            #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
            #[must_use]
            pub fn into_ucstring(self: Box<Self>) -> crate::$ucstring {
                let raw = Box::into_raw(self) as *mut [$uchar];
                crate::$ucstring {
                    inner: unsafe { Box::from_raw(raw) },
                }
            }

            /// Returns a wide string slice to this wide C string slice.
            ///
            /// The wide string slice will *not* include the nul-terminator.
            #[inline]
            #[must_use]
            pub fn as_ustr(&self) -> &$ustr {
                $ustr::from_slice(self.as_slice())
            }

            /// Returns a wide string slice to this wide C string slice.
            ///
            /// The wide string slice will include the nul-terminator.
            #[inline]
            #[must_use]
            pub fn as_ustr_with_nul(&self) -> &$ustr {
                $ustr::from_slice(self.as_slice_with_nul())
            }

            /// Returns a mutable wide string slice to this wide C string slice.
            ///
            /// The wide string slice will *not* include the nul-terminator.
            ///
            /// # Safety
            ///
            /// This method is unsafe because you can violate the invariants of this type when
            /// mutating the string (i.e. by adding interior nul values).
            #[inline]
            #[must_use]
            pub unsafe fn as_mut_ustr(&mut self) -> &mut $ustr {
                $ustr::from_slice_mut(self.as_mut_slice())
            }

            #[cfg(feature = "alloc")]
            pub(crate) fn from_inner(slice: &[$uchar]) -> &$ucstr {
                let ptr: *const [$uchar] = slice;
                unsafe { &*(ptr as *const $ucstr) }
            }

            #[cfg(feature = "alloc")]
            pub(crate) fn from_inner_mut(slice: &mut [$uchar]) -> &mut $ucstr {
                let ptr: *mut [$uchar] = slice;
                unsafe { &mut *(ptr as *mut $ucstr) }
            }

            $(#[$display_meta])*
            #[inline]
            #[must_use]
            pub fn display(&self) -> Display<'_, $ucstr> {
                Display { str: self }
            }

            /// Returns a subslice of the string.
            ///
            /// This is the non-panicking alternative to indexing the string. Returns [`None`]
            /// whenever equivalent indexing operation would panic.
            #[inline]
            #[must_use]
            pub fn get<I>(&self, i: I) -> Option<&$ustr>
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                self.as_slice().get(i).map($ustr::from_slice)
            }

            /// Returns a mutable subslice of the string.
            ///
            /// This is the non-panicking alternative to indexing the string. Returns [`None`]
            /// whenever equivalent indexing operation would panic.
            ///
            /// # Safety
            ///
            /// This method is unsafe because you can violate the invariants of this type when
            /// mutating the memory the pointer points to (i.e. by adding interior nul values).
            #[inline]
            #[must_use]
            pub unsafe fn get_mut<I>(&mut self, i: I) -> Option<&mut $ustr>
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                self.as_mut_slice().get_mut(i).map($ustr::from_slice_mut)
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
            pub unsafe fn get_unchecked<I>(&self, i: I) -> &$ustr
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                $ustr::from_slice(self.as_slice().get_unchecked(i))
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
            ///
            /// This method is unsafe because you can violate the invariants of this type when
            /// mutating the memory the pointer points to (i.e. by adding interior nul values).
            #[inline]
            #[must_use]
            pub unsafe fn get_unchecked_mut<I>(&mut self, i: I) -> &mut $ustr
            where
                I: SliceIndex<[$uchar], Output = [$uchar]>,
            {
                $ustr::from_slice_mut(self.as_mut_slice().get_unchecked_mut(i))
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
            pub fn split_at(&self, mid: usize) -> (&$ustr, &$ustr) {
                let split = self.as_slice().split_at(mid);
                ($ustr::from_slice(split.0), $ustr::from_slice(split.1))
            }

            /// Divide one mutable string slice into two at an index.
            ///
            /// The argument, `mid`, should be an offset from the start of the string.
            ///
            /// The two slices returned go from the start of the string slice to `mid`, and from
            /// `mid` to the end of the string slice.
            ///
            /// To get immutable string slices instead, see the [`split_at`][Self::split_at] method.
            ///
            /// # Safety
            ///
            /// This method is unsafe because you can violate the invariants of this type when
            /// mutating the memory the pointer points to (i.e. by adding interior nul values).
            #[inline]
            #[must_use]
            pub unsafe fn split_at_mut(&mut self, mid: usize) -> (&mut $ustr, &mut $ustr) {
                let split = self.as_mut_slice().split_at_mut(mid);
                ($ustr::from_slice_mut(split.0), $ustr::from_slice_mut(split.1))
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
            pub fn repeat(&self, n: usize) -> crate::$ucstring {
                unsafe { crate::$ucstring::from_vec_unchecked(self.as_slice().repeat(n)) }
            }
        }

        impl AsMut<$ucstr> for $ucstr {
            #[inline]
            fn as_mut(&mut self) -> &mut $ucstr {
                self
            }
        }

        impl AsRef<$ucstr> for $ucstr {
            #[inline]
            fn as_ref(&self) -> &Self {
                self
            }
        }

        impl AsRef<[$uchar]> for $ucstr {
            #[inline]
            fn as_ref(&self) -> &[$uchar] {
                self.as_slice()
            }
        }

        impl AsRef<$ustr> for $ucstr {
            #[inline]
            fn as_ref(&self) -> &$ustr {
                self.as_ustr()
            }
        }

        impl<'a> Default for &'a $ucstr {
            #[inline]
            fn default() -> Self {
                const SLICE: &[$uchar] = &[$ucstr::NUL_TERMINATOR];
                unsafe { $ucstr::from_slice_unchecked(SLICE) }
            }
        }

        #[cfg(feature = "alloc")]
        impl Default for Box<$ucstr> {
            #[inline]
            fn default() -> Box<$ucstr> {
                let boxed: Box<[$uchar]> = Box::from([$ucstr::NUL_TERMINATOR]);
                unsafe { Box::from_raw(Box::into_raw(boxed) as *mut $ucstr) }
            }
        }

        #[cfg(feature = "alloc")]
        impl<'a> From<&'a $ucstr> for Box<$ucstr> {
            #[inline]
            fn from(s: &'a $ucstr) -> Box<$ucstr> {
                let boxed: Box<[$uchar]> = Box::from(s.as_slice_with_nul());
                unsafe { Box::from_raw(Box::into_raw(boxed) as *mut $ucstr) }
            }
        }

        #[cfg(feature = "std")]
        impl From<&$ucstr> for std::ffi::OsString {
            #[inline]
            fn from(s: &$ucstr) -> std::ffi::OsString {
                s.to_os_string()
            }
        }

        impl<I> Index<I> for $ucstr
        where
            I: SliceIndex<[$uchar], Output = [$uchar]>,
        {
            type Output = $ustr;

            #[inline]
            fn index(&self, index: I) -> &Self::Output {
                $ustr::from_slice(&self.as_slice()[index])
            }
        }

        impl PartialEq<$ucstr> for &$ucstr {
            #[inline]
            fn eq(&self, other: &$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<&$ucstr> for $ucstr {
            #[inline]
            fn eq(&self, other: &&$ucstr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$ustr> for $ucstr {
            #[inline]
            fn eq(&self, other: &$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<$ustr> for &$ucstr {
            #[inline]
            fn eq(&self, other: &$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialEq<&$ustr> for $ucstr {
            #[inline]
            fn eq(&self, other: &&$ustr) -> bool {
                self.as_slice() == other.as_slice()
            }
        }

        impl PartialOrd<$ustr> for $ucstr {
            #[inline]
            fn partial_cmp(&self, other: &$ustr) -> Option<core::cmp::Ordering> {
                self.as_ustr().partial_cmp(other)
            }
        }
    };
}

ucstr_common_impl! {
    /// C-style 16-bit wide string slice for [`U16CString`][crate::U16CString].
    ///
    /// [`U16CStr`] is to [`U16CString`][crate::U16CString] as [`CStr`][std::ffi::CStr] is to
    /// [`CString`][std::ffi::CString].
    ///
    /// [`U16CStr`] are string slices that do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-16 data, they may be used for
    /// any wide encoded string.
    ///
    /// # Nul termination
    ///
    /// [`U16CStr`] is aware of nul (`0`) values. Unless unchecked conversions are used, all
    /// [`U16CStr`] strings end with a nul-terminator in the underlying buffer and contain no
    /// internal nul values. These strings are intended to be used with C FFI functions that
    /// require nul-terminated strings.
    ///
    /// Because of the nul termination requirement, multiple classes methods for provided for
    /// construction a [`U16CStr`] under various scenarios. By default, methods such as
    /// [`from_ptr`][Self::from_ptr] and [`from_slice`][Self::from_slice] return an error if the
    /// input does not terminate with a nul value, or if it contains any interior nul values before
    /// the terminator.
    ///
    /// `_truncate` methods on the other hand, such as
    /// [`from_ptr_truncate`][Self::from_ptr_truncate] and
    /// [`from_slice_truncate`][Self::from_slice_truncate], construct a slice that terminates with
    /// the first nul value encountered in the string, only returning an error if the slice contains
    /// no nul values at all. Use this to mimic the behavior of C functions such as `strlen` when
    /// you don't know if the input is clean of interior nuls.
    ///
    /// Finally, unsafe `_unchecked` variants of these methods, such as
    /// [`from_ptr_unchecked`][Self::from_ptr_unchecked] and
    /// [`from_slice_unchecked`][Self::from_slice_unchecked] allow bypassing any checks for nul
    /// values, when the input has already been ensured to have a nul terminator and no interior
    /// nul values.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U16CStr`] outside of FFI is with the [`u16cstr!`][crate::u16cstr]
    /// macro to convert string literals into nul-terminated UTF-16 string slices at compile time:
    ///
    /// ```
    /// use widestring::u16cstr;
    /// let hello = u16cstr!("Hello, world!");
    /// ```
    ///
    /// You can also convert any [`u16`] slice directly, as long as it has a nul terminator:
    ///
    /// ```
    /// use widestring::{u16cstr, U16CStr};
    ///
    /// let sparkle_heart = [0xd83d, 0xdc96, 0x0];
    /// let sparkle_heart = U16CStr::from_slice(&sparkle_heart).unwrap();
    ///
    /// assert_eq!(u16cstr!("💖"), sparkle_heart);
    ///
    /// // This unpaired UTf-16 surrogate is invalid UTF-16, but is perfectly valid in U16CStr
    /// let malformed_utf16 = [0xd83d, 0x0];
    /// let s = U16CStr::from_slice(&malformed_utf16).unwrap();
    ///
    /// assert_eq!(s.len(), 1);
    /// ```
    ///
    /// When working with a FFI, it is useful to create a [`U16CStr`] from a pointer:
    ///
    /// ```
    /// use widestring::{u16cstr, U16CStr};
    ///
    /// let sparkle_heart = [0xd83d, 0xdc96, 0x0];
    /// let s = unsafe {
    ///     // Note the string and pointer length does not include the nul terminator
    ///     U16CStr::from_ptr(sparkle_heart.as_ptr(), sparkle_heart.len() - 1).unwrap()
    /// };
    /// assert_eq!(u16cstr!("💖"), s);
    ///
    /// // Alternatively, if the length of the pointer is unknown but definitely terminates in nul,
    /// // a C-style string version can be used
    /// let s = unsafe { U16CStr::from_ptr_str(sparkle_heart.as_ptr()) };
    ///
    /// assert_eq!(u16cstr!("💖"), s);
    /// ```
    struct U16CStr([u16]);

    type UCString = U16CString;
    type UStr = U16Str;
    type UString = U16String;

    /// Copies the string reference to a new owned wide string.
    ///
    /// The resulting wide string will **not** have a nul terminator.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let wcstr = U16CString::from_str("MyString").unwrap();
    /// // Convert U16CString to a U16String
    /// let wstr = wcstr.to_ustring();
    ///
    /// // U16CString will have a terminating nul
    /// let wcvec = wcstr.into_vec_with_nul();
    /// assert_eq!(wcvec[wcvec.len()-1], 0);
    /// // The resulting U16String will not have the terminating nul
    /// let wvec = wstr.into_vec();
    /// assert_ne!(wvec[wvec.len()-1], 0);
    /// ```
    fn to_ustring() -> {}

    /// Converts a boxed wide C string slice into an wide C string without copying or
    /// allocating.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::U16CString;
    ///
    /// let v = vec![102u16, 111u16, 111u16]; // "foo"
    /// let c_string = U16CString::from_vec(v.clone()).unwrap();
    /// let boxed = c_string.into_boxed_ucstr();
    /// assert_eq!(boxed.into_ucstring(), U16CString::from_vec(v).unwrap());
    /// ```
    fn into_ucstring() -> {}

    /// Returns an object that implements [`Display`][std::fmt::Display] for printing
    /// strings that may contain non-Unicode data.
    ///
    /// A wide C string might data of any encoding. This function assumes the string is encoded in
    /// UTF-16, and returns a struct implements the
    /// [`Display`][std::fmt::Display] trait in a way that decoding the string is lossy but
    /// no heap allocations are performed, such as by
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
    /// use widestring::U16CStr;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U16CStr::from_slice(&[
    ///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834, 0x0000,
    /// ]).unwrap();
    ///
    /// assert_eq!(format!("{}", s.display()),
    /// "𝄞mus�ic�"
    /// );
    /// ```
    ///
    /// Using alternate formatting style to skip invalid values entirely:
    ///
    /// ```
    /// use widestring::U16CStr;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U16CStr::from_slice(&[
    ///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834, 0x0000,
    /// ]).unwrap();
    ///
    /// assert_eq!(format!("{:#}", s.display()),
    /// "𝄞music"
    /// );
    /// ```
    fn display() -> {}
}

ucstr_common_impl! {
    /// C-style 32-bit wide string slice for [`U32CString`][crate::U32CString].
    ///
    /// [`U32CStr`] is to [`U32CString`][crate::U32CString] as [`CStr`][std::ffi::CStr] is to
    /// [`CString`][std::ffi::CString].
    ///
    /// [`U32CStr`] are string slices that do not have a defined encoding. While it is sometimes
    /// assumed that they contain possibly invalid or ill-formed UTF-32 data, they may be used for
    /// any wide encoded string.
    ///
    /// # Nul termination
    ///
    /// [`U32CStr`] is aware of nul (`0`) values. Unless unchecked conversions are used, all
    /// [`U32CStr`] strings end with a nul-terminator in the underlying buffer and contain no
    /// internal nul values. These strings are intended to be used with C FFI functions that
    /// require nul-terminated strings.
    ///
    /// Because of the nul termination requirement, multiple classes methods for provided for
    /// construction a [`U32CStr`] under various scenarios. By default, methods such as
    /// [`from_ptr`][Self::from_ptr] and [`from_slice`][Self::from_slice] return an error if the
    /// input does not terminate with a nul value, or if it contains any interior nul values before
    /// the terminator.
    ///
    /// `_truncate` methods on the other hand, such as
    /// [`from_ptr_truncate`][Self::from_ptr_truncate] and
    /// [`from_slice_truncate`][Self::from_slice_truncate], construct a slice that terminates with
    /// the first nul value encountered in the string, only returning an error if the slice contains
    /// no nul values at all. Use this to mimic the behavior of C functions such as `strlen` when
    /// you don't know if the input is clean of interior nuls.
    ///
    /// Finally, unsafe `_unchecked` variants of these methods, such as
    /// [`from_ptr_unchecked`][Self::from_ptr_unchecked] and
    /// [`from_slice_unchecked`][Self::from_slice_unchecked] allow bypassing any checks for nul
    /// values, when the input has already been ensured to have a nul terminator and no interior
    /// nul values.
    ///
    /// # Examples
    ///
    /// The easiest way to use [`U32CStr`] outside of FFI is with the [`u32cstr!`][crate::u32cstr]
    /// macro to convert string literals into nul-terminated UTF-32 string slices at compile time:
    ///
    /// ```
    /// use widestring::u32cstr;
    /// let hello = u32cstr!("Hello, world!");
    /// ```
    ///
    /// You can also convert any [`u32`] slice directly, as long as it has a nul terminator:
    ///
    /// ```
    /// use widestring::{u32cstr, U32CStr};
    ///
    /// let sparkle_heart = [0x1f496, 0x0];
    /// let sparkle_heart = U32CStr::from_slice(&sparkle_heart).unwrap();
    ///
    /// assert_eq!(u32cstr!("💖"), sparkle_heart);
    ///
    /// // This UTf-16 surrogate is invalid UTF-32, but is perfectly valid in U32CStr
    /// let malformed_utf32 = [0xd83d, 0x0];
    /// let s = U32CStr::from_slice(&malformed_utf32).unwrap();
    ///
    /// assert_eq!(s.len(), 1);
    /// ```
    ///
    /// When working with a FFI, it is useful to create a [`U32CStr`] from a pointer:
    ///
    /// ```
    /// use widestring::{u32cstr, U32CStr};
    ///
    /// let sparkle_heart = [0x1f496, 0x0];
    /// let s = unsafe {
    ///     // Note the string and pointer length does not include the nul terminator
    ///     U32CStr::from_ptr(sparkle_heart.as_ptr(), sparkle_heart.len() - 1).unwrap()
    /// };
    /// assert_eq!(u32cstr!("💖"), s);
    ///
    /// // Alternatively, if the length of the pointer is unknown but definitely terminates in nul,
    /// // a C-style string version can be used
    /// let s = unsafe { U32CStr::from_ptr_str(sparkle_heart.as_ptr()) };
    ///
    /// assert_eq!(u32cstr!("💖"), s);
    /// ```
    struct U32CStr([u32]);

    type UCString = U32CString;
    type UStr = U32Str;
    type UString = U32String;

    /// Copies the string reference to a new owned wide string.
    ///
    /// The resulting wide string will **not** have a nul terminator.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let wcstr = U32CString::from_str("MyString").unwrap();
    /// // Convert U32CString to a U32String
    /// let wstr = wcstr.to_ustring();
    ///
    /// // U32CString will have a terminating nul
    /// let wcvec = wcstr.into_vec_with_nul();
    /// assert_eq!(wcvec[wcvec.len()-1], 0);
    /// // The resulting U32String will not have the terminating nul
    /// let wvec = wstr.into_vec();
    /// assert_ne!(wvec[wvec.len()-1], 0);
    /// ```
    fn to_ustring() -> {}

    /// Converts a boxed wide C string slice into an owned wide C string without copying or
    /// allocating.
    ///
    /// # Examples
    ///
    /// ```
    /// use widestring::U32CString;
    ///
    /// let v = vec![102u32, 111u32, 111u32]; // "foo"
    /// let c_string = U32CString::from_vec(v.clone()).unwrap();
    /// let boxed = c_string.into_boxed_ucstr();
    /// assert_eq!(boxed.into_ucstring(), U32CString::from_vec(v).unwrap());
    /// ```
    fn into_ucstring() -> {}

    /// Returns an object that implements [`Display`][std::fmt::Display] for printing
    /// strings that may contain non-Unicode data.
    ///
    /// A wide C string might data of any encoding. This function assumes the string is encoded in
    /// UTF-32, and returns a struct implements the
    /// [`Display`][std::fmt::Display] trait in a way that decoding the string is lossy but
    /// no heap allocations are performed, such as by
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
    /// use widestring::U32CStr;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U32CStr::from_slice(&[
    ///     0x1d11e, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834, 0x0000,
    /// ]).unwrap();
    ///
    /// assert_eq!(format!("{}", s.display()),
    /// "𝄞mus�ic�"
    /// );
    /// ```
    ///
    /// Using alternate formatting style to skip invalid values entirely:
    ///
    /// ```
    /// use widestring::U32CStr;
    ///
    /// // 𝄞mus<invalid>ic<invalid>
    /// let s = U32CStr::from_slice(&[
    ///     0x1d11e, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834, 0x0000,
    /// ]).unwrap();
    ///
    /// assert_eq!(format!("{:#}", s.display()),
    /// "𝄞music"
    /// );
    /// ```
    fn display() -> {}
}

impl U16CStr {
    /// Copys a string to an owned [`OsString`][std::ffi::OsString].
    ///
    /// This makes a string copy of the [`U16CStr`]. Since [`U16CStr`] makes no guarantees that it
    /// is valid UTF-16, there is no guarantee that the resulting [`OsString`][std::ffi::OsString]
    /// will be valid data. The [`OsString`][std::ffi::OsString] will **not** have a nul
    /// terminator.
    ///
    /// Note that the encoding of [`OsString`][std::ffi::OsString] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms (such as
    /// windows) no changes to the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// use std::ffi::OsString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16CString::from_str(s).unwrap();
    /// // Create an OsString from the wide string
    /// let osstr = wstr.to_os_string();
    ///
    /// assert_eq!(osstr, OsString::from(s));
    /// ```
    #[inline]
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[must_use]
    pub fn to_os_string(&self) -> std::ffi::OsString {
        crate::platform::os_from_wide(self.as_slice())
    }

    /// Copies the string to a [`String`] if it contains valid UTF-16 data.
    ///
    /// This method assumes this string is encoded as UTF-16 and attempts to decode it as such. It
    /// will **not* have a nul terminator.
    ///
    /// # Errors
    ///
    /// Returns an error if the string contains any invalid UTF-16 data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16CString::from_str(s).unwrap();
    /// // Create a regular string from the wide string
    /// let s2 = wstr.to_string().unwrap();
    ///
    /// assert_eq!(s2, s);
    /// ```
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    pub fn to_string(&self) -> Result<String, crate::error::Utf16Error> {
        self.as_ustr().to_string()
    }

    /// Decodes the string reference to a [`String`] even if it is invalid UTF-16 data.
    ///
    /// This method assumes this string is encoded as UTF-16 and attempts to decode it as such. Any
    /// invalid sequences are replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which looks like this:
    /// �. It will **not* have a nul terminator.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U16CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U16CString::from_str(s).unwrap();
    /// // Create a regular string from the wide string
    /// let s2 = wstr.to_string_lossy();
    ///
    /// assert_eq!(s2, s);
    /// ```
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(self.as_slice())
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
    /// is an iterator over `Result<char, DecodeUtf16Error>` as well as their positions, instead of
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
}

impl U32CStr {
    /// Constructs a string reference from a [`char`] nul-terminated string pointer.
    ///
    /// This will scan for nul values beginning with `p`. The first nul value will be used as the
    /// nul terminator for the string, similar to how libc string functions such as `strlen` work.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid or has a
    /// nul terminator, and the function could scan past the underlying buffer.
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
    pub unsafe fn from_char_ptr_str<'a>(p: *const char) -> &'a Self {
        Self::from_ptr_str(p as *const u32)
    }

    /// Constructs a mutable string reference from a mutable [`char`] nul-terminated string pointer.
    ///
    /// This will scan for nul values beginning with `p`. The first nul value will be used as the
    /// nul terminator for the string, similar to how libc string functions such as `strlen` work.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid or has a
    /// nul terminator, and the function could scan past the underlying buffer.
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
    pub unsafe fn from_char_ptr_str_mut<'a>(p: *mut char) -> &'a mut Self {
        Self::from_ptr_str_mut(p as *mut u32)
    }

    /// Constructs a string reference from a [`char`] pointer and a length.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes, and does
    /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and means
    /// that `p` is a pointer directly to the nul terminator of the string.
    ///
    /// # Errors
    ///
    /// This will scan the pointer string for an interior nul value and error if one is found
    /// before the nul terminator at `len` offset. To avoid scanning for interior nuls,
    /// [`from_ptr_unchecked`][Self::from_ptr_unchecked] may be used instead.
    ///
    /// An error is returned if the value at `len` offset is not a nul terminator.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len +
    /// 1` elements.
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
    pub unsafe fn from_char_ptr<'a>(p: *const char, len: usize) -> Result<&'a Self, NulError<u32>> {
        Self::from_ptr(p as *const u32, len)
    }

    /// Constructs a mutable string reference from a mutable [`char`] pointer and a length.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes, and does
    /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and means
    /// that `p` is a pointer directly to the nul terminator of the string.
    ///
    /// # Errors
    ///
    /// This will scan the pointer string for an interior nul value and error if one is found
    /// before the nul terminator at `len` offset. To avoid scanning for interior nuls,
    /// [`from_ptr_unchecked_mut`][Self::from_ptr_unchecked_mut] may be used instead.
    ///
    /// An error is returned if the value at `len` offset is not a nul terminator.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len +
    /// 1` elements.
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
    pub unsafe fn from_char_ptr_mut<'a>(
        p: *mut char,
        len: usize,
    ) -> Result<&'a mut Self, NulError<u32>> {
        Self::from_ptr_mut(p as *mut u32, len)
    }

    /// Constructs a string reference from a [`char`] pointer and a length, truncating at the first
    /// nul terminator.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes. This will scan
    /// for nul values beginning with `p` until offset `len`. The first nul value will be used as
    /// the nul terminator for the string, ignoring any remaining values left before `len`.
    ///
    /// # Errors
    ///
    /// If no nul terminator is found after `len` + 1 elements, an error is returned.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid or has a
    /// nul terminator, and the function could scan past the underlying buffer.
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
    pub unsafe fn from_char_ptr_truncate<'a>(
        p: *const char,
        len: usize,
    ) -> Result<&'a Self, MissingNulTerminator> {
        Self::from_ptr_truncate(p as *const u32, len)
    }

    /// Constructs a mutable string reference from a mutable [`char`] pointer and a length,
    /// truncating at the first nul terminator.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes. This will scan
    /// for nul values beginning with `p` until offset `len`. The first nul value will be used as
    /// the nul terminator for the string, ignoring any remaining values left before `len`.
    ///
    /// # Errors
    ///
    /// If no nul terminator is found after `len` + 1 elements, an error is returned.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid or has a
    /// nul terminator, and the function could scan past the underlying buffer.
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
    pub unsafe fn from_char_ptr_truncate_mut<'a>(
        p: *mut char,
        len: usize,
    ) -> Result<&'a mut Self, MissingNulTerminator> {
        Self::from_ptr_truncate_mut(p as *mut u32, len)
    }

    /// Constructs a string reference from a [`char`] pointer and a length without checking for any
    /// nul values.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes, and does
    /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and means
    /// that `p` is a pointer directly to the nul terminator of the string.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len +
    /// 1` elements, nor that is has a terminating nul value.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts].
    /// In particular, the returned string reference *must not be mutated* for the duration of
    /// lifetime `'a`, except inside an [`UnsafeCell`][std::cell::UnsafeCell].
    ///
    /// The interior values of the pointer are not scanned for nul. Any interior nul values or
    /// a missing nul terminator at pointer offset `len` + 1 will result in an invalid string slice.
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
    pub unsafe fn from_char_ptr_unchecked<'a>(p: *const char, len: usize) -> &'a Self {
        Self::from_ptr_unchecked(p as *const u32, len)
    }

    /// Constructs a mutable string reference from a mutable [`char`] pointer and a length without
    /// checking for any nul values.
    ///
    /// The `len` argument is the number of elements, **not** the number of bytes, and does
    /// **not** include the nul terminator of the string. Thus, a `len` of 0 is valid and means
    /// that `p` is a pointer directly to the nul terminator of the string.
    ///
    /// # Safety
    ///
    /// This function is unsafe as there is no guarantee that the given pointer is valid for `len +
    /// 1` elements, nor that is has a terminating nul value.
    ///
    /// In addition, the data must meet the safety conditions of [std::slice::from_raw_parts_mut].
    ///
    /// The interior values of the pointer are not scanned for nul. Any interior nul values or
    /// a missing nul terminator at pointer offset `len` + 1 will result in an invalid string slice.
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
    pub unsafe fn from_char_ptr_unchecked_mut<'a>(p: *mut char, len: usize) -> &'a mut Self {
        Self::from_ptr_unchecked_mut(p as *mut u32, len)
    }

    /// Constructs a string reference from a [`char`] slice with a terminating nul, checking for
    /// invalid interior nul values.
    ///
    /// The slice must have at least one item, the nul terminator, even for an empty string.
    ///
    /// # Errors
    ///
    /// If there are nul values in the slice except for the last value, an error is returned.
    ///
    /// An error is also returned if the last value of the slice is not a nul terminator.
    pub fn from_char_slice(slice: &[char]) -> Result<&Self, NulError<u32>> {
        let ptr: *const [char] = slice;
        Self::from_slice(unsafe { &*(ptr as *const [u32]) })
    }

    /// Constructs a mutable string reference from a mutable [`char`] slice with a terminating nul,
    /// checking for invalid interior nul values.
    ///
    /// The slice must have at least one item, the nul terminator, even for an empty string.
    ///
    /// # Errors
    ///
    /// If there are nul values in the slice except for the last value, an error is returned.
    ///
    /// An error is also returned if the last value of the slice is not a nul terminator.
    pub fn from_char_slice_mut(slice: &mut [char]) -> Result<&mut Self, NulError<u32>> {
        let ptr: *mut [char] = slice;
        Self::from_slice_mut(unsafe { &mut *(ptr as *mut [u32]) })
    }

    /// Constructs a string reference from a slice of [`char`] values, truncating at the first nul
    /// terminator.
    ///
    /// The slice will be scanned for nul values. When a nul value is found, it is treated as the
    /// terminator for the string, and the string slice will be truncated to that nul.
    ///
    /// # Errors
    ///
    /// If there are no nul values in the slice, an error is returned.
    #[inline]
    pub fn from_char_slice_truncate(slice: &[char]) -> Result<&Self, MissingNulTerminator> {
        let ptr: *const [char] = slice;
        Self::from_slice_truncate(unsafe { &*(ptr as *const [u32]) })
    }

    /// Constructs a mutable string reference from a mutable slice of [`char`] values, truncating at
    /// the first nul terminator.
    ///
    /// The slice will be scanned for nul values. When a nul value is found, it is treated as the
    /// terminator for the string, and the string slice will be truncated to that nul.
    ///
    /// # Errors
    ///
    /// If there are no nul values in the slice, an error is returned.
    #[inline]
    pub fn from_char_slice_truncate_mut(
        slice: &mut [char],
    ) -> Result<&mut Self, MissingNulTerminator> {
        let ptr: *mut [char] = slice;
        Self::from_slice_truncate_mut(unsafe { &mut *(ptr as *mut [u32]) })
    }

    /// Constructs a string reference from a [`char`] slice without checking for a terminating or
    /// interior nul values.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it can lead to invalid C string slice values when the slice
    /// is missing a terminating nul value or there are non-terminating interior nul values
    /// in the slice. In particular, an empty slice will result in an invalid string slice.
    #[inline]
    #[must_use]
    pub unsafe fn from_char_slice_unchecked(slice: &[char]) -> &Self {
        let ptr: *const [char] = slice;
        Self::from_slice_unchecked(&*(ptr as *const [u32]))
    }

    /// Constructs a mutable string reference from a mutable [`char`] slice without checking for a
    /// terminating or interior nul values.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it can lead to invalid C string slice values when the slice
    /// is missing a terminating nul value or there are non-terminating interior nul values
    /// in the slice. In particular, an empty slice will result in an invalid string slice.
    #[inline]
    #[must_use]
    pub unsafe fn from_char_slice_unchecked_mut(slice: &mut [char]) -> &mut Self {
        let ptr: *mut [char] = slice;
        Self::from_slice_unchecked_mut(&mut *(ptr as *mut [u32]))
    }

    /// Decodes a string reference to an owned [`OsString`][std::ffi::OsString].
    ///
    /// This makes a string copy of this reference. Since [`U32CStr`] makes no guarantees that it
    /// is valid UTF-32, there is no guarantee that the resulting [`OsString`][std::ffi::OsString]
    /// will be valid data. The [`OsString`][std::ffi::OsString] will **not** have a nul
    /// terminator.
    ///
    /// Note that the encoding of [`OsString`][std::ffi::OsString] is platform-dependent, so on
    /// some platforms this may make an encoding conversions, while on other platforms no changes to
    /// the string will be made.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// use std::ffi::OsString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32CString::from_str(s).unwrap();
    /// // Create an OsString from the wide string
    /// let osstr = wstr.to_os_string();
    ///
    /// assert_eq!(osstr, OsString::from(s));
    /// ```
    #[inline]
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    #[must_use]
    pub fn to_os_string(&self) -> std::ffi::OsString {
        self.as_ustr().to_os_string()
    }

    /// Decodes the string reference to a [`String`] if it contains valid UTF-32 data.
    ///
    /// This method assumes this string is encoded as UTF-32 and attempts to decode it as such. It
    /// will **not* have a nul terminator.
    ///
    /// # Errors
    ///
    /// Returns an error if the string contains any invalid UTF-32 data.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32CString::from_str(s).unwrap();
    /// // Create a regular string from the wide string
    /// let s2 = wstr.to_string().unwrap();
    ///
    /// assert_eq!(s2, s);
    /// ```
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    pub fn to_string(&self) -> Result<String, crate::error::Utf32Error> {
        self.as_ustr().to_string()
    }

    /// Decodes the string reference to a [`String`] even if it is invalid UTF-32 data.
    ///
    /// This method assumes this string is encoded as UTF-16 and attempts to decode it as such. Any
    /// invalid sequences are replaced with
    /// [`U+FFFD REPLACEMENT CHARACTER`][core::char::REPLACEMENT_CHARACTER], which looks like this:
    /// �. It will **not* have a nul terminator.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use widestring::U32CString;
    /// let s = "MyString";
    /// // Create a wide string from the string
    /// let wstr = U32CString::from_str(s).unwrap();
    /// // Create a regular string from the wide string
    /// let s2 = wstr.to_string_lossy();
    ///
    /// assert_eq!(s2, s);
    /// ```
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        self.as_ustr().to_string_lossy()
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
    /// may consist of invalid UTF-32, the iterator returned by this method will replace invalid
    /// data with
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
}

impl core::fmt::Debug for U16CStr {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u16(self.as_slice_with_nul(), f)
    }
}

impl core::fmt::Debug for U32CStr {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        crate::debug_fmt_u32(self.as_slice_with_nul(), f)
    }
}

/// Alias for [`U16CStr`] or [`U32CStr`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(not(windows))]
pub type WideCStr = U32CStr;

/// Alias for [`U16CStr`] or [`U32CStr`] depending on platform. Intended to match typical C
/// `wchar_t` size on platform.
#[cfg(windows)]
pub type WideCStr = U16CStr;

/// Helper struct for printing wide C string values with [`format!`] and `{}`.
///
/// A wide C string might contain ill-formed UTF encoding. This struct implements the
/// [`Display`][std::fmt::Display] trait in a way that decoding the string is lossy but no heap
/// allocations are performed, such as by [`to_string_lossy`][U16CStr::to_string_lossy]. It is
/// created by the [`display`][U16CStr::display] method on [`U16CStr`] and [`U32CStr`].
///
/// By default, invalid Unicode data is replaced with
/// [`U+FFFD REPLACEMENT CHARACTER`][std::char::REPLACEMENT_CHARACTER] (�). If you wish to simply
/// skip any invalid Uncode data and forego the replacement, you may use the
/// [alternate formatting][std::fmt#sign0] with `{:#}`.
pub struct Display<'a, S: ?Sized> {
    str: &'a S,
}

impl core::fmt::Debug for Display<'_, U16CStr> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.str, f)
    }
}

impl core::fmt::Debug for Display<'_, U32CStr> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.str, f)
    }
}

impl core::fmt::Display for Display<'_, U16CStr> {
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

impl core::fmt::Display for Display<'_, U32CStr> {
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
