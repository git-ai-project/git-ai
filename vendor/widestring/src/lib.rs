//! A wide string library for converting to and from wide string variants.
//!
//! This library provides multiple types of wide strings, each corresponding to a string types in
//! the Rust standard library. [`Utf16String`] and [`Utf32String`] are analogous to the standard
//! [`String`] type, providing a similar interface, and are always encoded as valid UTF-16 and
//! UTF-32, respectively. They are the only type in this library that can losslessly and infallibly
//! convert to and from [`String`], and are the easiest type to work with. They are not designed for
//! working with FFI, but do support efficient conversions from the FFI types.
//!
//! [`U16String`] and [`U32String`], on the other hand, are similar to (but not the same as),
//! [`OsString`], and are designed around working with FFI. Unlike the UTF variants, these strings
//! do not have a defined encoding, and can work with any wide character strings, regardless of
//! the encoding. They can be converted to and from [`OsString`] (but may require an encoding
//! conversion depending on the platform), although that string type is an OS-specified
//! encoding, so take special care.
//!
//! [`U16String`] and [`U32String`] also allow access and mutation that relies on the user
//! to enforce any constraints on the data. Some methods do assume a UTF encoding, but do so in a
//! way that handles malformed encoding data. For FFI, use [`U16String`] or [`U32String`] when you
//! simply need to pass-through string data, or when you're not dealing with a nul-terminated data.
//!
//! Finally, [`U16CString`] and [`U32CString`] are wide version of the standard [`CString`] type.
//! Like [`U16String`] and [`U32String`], they do not have defined encoding, but are designed to
//! work with FFI, particularly C-style nul-terminated wide string data. These C-style strings are
//! always terminated in a nul value, and are guaranteed to contain no interior nul values (unless
//! unchecked methods are used). Again, these types may contain ill-formed encoding data, and
//! methods handle it appropriately. Use [`U16CString`] or [`U32CString`] anytime you must properly
//! handle nul values for when dealing with wide string C FFI.
//!
//! Like the standard Rust string types, each wide string type has its corresponding wide string
//! slice type, as shown in the following table:
//!
//! | String Type     | Slice Type   |
//! |-----------------|--------------|
//! | [`Utf16String`] | [`Utf16Str`] |
//! | [`Utf32String`] | [`Utf32Str`] |
//! | [`U16String`]   | [`U16Str`]   |
//! | [`U32String`]   | [`U32Str`]   |
//! | [`U16CString`]  | [`U16CStr`]  |
//! | [`U32CString`]  | [`U32CStr`]  |
//!
//! All the string types in this library can be converted between string types of the same bit
//! width, as well as appropriate standard Rust types, but be lossy and/or require knowledge of the
//! underlying encoding. The UTF strings additionally can be converted between the two sizes of
//! string, re-encoding the strings.
//!
//! # Wide string literals
//!
//! Macros are provided for each wide string slice type that convert standard Rust [`str`] literals
//! into UTF-16 or UTF-32 encoded versions of the slice type at *compile time*.
//!
//! ```
//! use widestring::u16str;
//! let hello = u16str!("Hello, world!"); // `hello` will be a &U16Str value
//! ```
//!
//! These can be used anywhere a `const` function can be used, and provide a convenient method of
//! specifying wide string literals instead of coding values by hand. The resulting string slices
//! are always valid UTF encoding, and the [`u16cstr!`] and [`u32cstr!`] macros are automatically
//! nul-terminated.
//!
//! # Cargo features
//!
//! This crate supports `no_std` when default cargo features are disabled. The `std` and `alloc`
//! cargo features (enabled by default) enable the owned string types: [`U16String`], [`U32String`],
//! [`U16CString`], [`U32CString`], [`Utf16String`], and [`Utf32String`] types and their modules.
//! Other types such as the string slices do not require allocation and can be used in a `no_std`
//! environment, even without the [`alloc`](https://doc.rust-lang.org/stable/alloc/index.html)
//! crate.
//!
//! # Remarks on UTF-16 and UTF-32
//!
//! UTF-16 encoding is a variable-length encoding. The 16-bit code units can specificy Unicode code
//! points either as single units or in _surrogate pairs_. Because every value might be part of a
//! surrogate pair, many regular string operations on UTF-16 data, including indexing, writing, or
//! even iterating, require considering either one or two values at a time. This library provides
//! safe methods for these operations when the data is known to be UTF-16, such as with
//! [`Utf16String`]. In those cases, keep in mind that the number of elements (`len()`) of the
//! wide string is _not_ equivalent to the number of Unicode code points in the string, but is
//! instead the number of code unit values.
//!
//! For [`U16String`] and [`U16CString`], which do not define an encoding, these same operations
//! (indexing, mutating, iterating) do _not_ take into account UTF-16 encoding and may result in
//! sequences that are ill-formed UTF-16. Some methods are provided that do make an exception to
//! this and treat the strings as malformed UTF-16, which are specified in their documentation as to
//! how they handle the invalid data.
//!
//! UTF-32 simply encodes Unicode code points as-is in 32-bit Unicode Scalar Values, but Unicode
//! character code points are reserved only for 21-bits, and UTF-16 surrogates are invalid in
//! UTF-32. Since UTF-32 is a fixed-width encoding, it is much easier to deal with, but equivalent
//! methods to the 16-bit strings are provided for compatibility.
//!
//! All the 32-bit wide strings provide efficient methods to convert to and from sequences of
//! [`char`] data, as the representation of UTF-32 strings is functionally equivalent to sequences
//! of [`char`]s. Keep in mind that only [`Utf32String`] guaruntees this equivalence, however, since
//! the other strings may contain invalid values.
//!
//! # FFI with C/C++ `wchar_t`
//!
//! C/C++'s `wchar_t` (and C++'s corresponding `widestring`) varies in size depending on compiler
//! and platform. Typically, `wchar_t` is 16-bits on Windows and 32-bits on most Unix-based
//! platforms. For convenience when using `wchar_t`-based FFI's, type aliases for the corresponding
//! string types are provided: [`WideString`] aliases [`U16String`] on Windows or [`U32String`]
//! elsewhere, [`WideCString`] aliases [`U16CString`] or [`U32CString`], and [`WideUtfString`]
//! aliases [`Utf16String`] or [`Utf32String`]. [`WideStr`], [`WideCStr`], and [`WideUtfStr`] are
//! provided for the string slice types. The [`WideChar`] alias is also provided, aliasing [`u16`]
//! or [`u32`] depending on platform.
//!
//! When not interacting with a FFI that uses `wchar_t`, it is recommended to use the string types
//! directly rather than via the wide alias.
//!
//! # Nul values
//!
//! This crate uses the term legacy ASCII term "nul" to refer to Unicode code point `U+0000 NULL`
//! and its associated code unit representation as zero-value bytes. This is to disambiguate this
//! zero value from null pointer values. C-style strings end in a nul value, while regular Rust
//! strings allow interior nul values and are not terminated with nul.
//!
//! # Examples
//!
//! The following example uses [`U16String`] to get Windows error messages, since `FormatMessageW`
//! returns a string length for us and we don't need to pass error messages into other FFI
//! functions so we don't need to worry about nul values.
//!
//! ```rust
//! # #[cfg(any(not(windows), not(feature = "alloc")))]
//! # fn main() {}
//! # extern crate windows_sys;
//! # extern crate widestring;
//! # #[cfg(all(windows, feature = "alloc"))]
//! # fn main() {
//! use windows_sys::{Win32::{System::Diagnostics::Debug::{FormatMessageW,
//!     FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_ALLOCATE_BUFFER,
//!     FORMAT_MESSAGE_IGNORE_INSERTS}, Foundation::{LocalFree, HLOCAL}}, core::PWSTR};
//! use std::ptr;
//! use widestring::U16String;
//!
//! let error_code: u32 = 0;
//! let s: U16String;
//! unsafe {
//!     // First, get a string buffer from some windows api such as FormatMessageW...
//!     let mut buffer: PWSTR = ptr::null_mut();
//!     let strlen = FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM |
//!                                 FORMAT_MESSAGE_ALLOCATE_BUFFER |
//!                                 FORMAT_MESSAGE_IGNORE_INSERTS,
//!                                 ptr::null(),
//!                                 error_code, // error code from GetLastError()
//!                                 0,
//!                                 (&mut buffer as *mut PWSTR) as PWSTR,
//!                                 0,
//!                                 ptr::null_mut());
//!
//!     // Get the buffer as a wide string
//!     s = U16String::from_ptr(buffer, strlen as usize);
//!     // Since U16String creates an owned copy, it's safe to free original buffer now
//!     // If you didn't want an owned copy, you could use &U16Str.
//!     LocalFree(buffer as HLOCAL);
//! }
//! // Convert to a regular Rust String and use it to your heart's desire!
//! let message = s.to_string_lossy();
//! # assert_eq!(message, "The operation completed successfully.\r\n");
//! # }
//! ```
//!
//! The following example is the functionally the same, only using [`U16CString`] instead.
//!
//! ```rust
//! # #[cfg(any(not(windows), not(feature = "alloc")))]
//! # fn main() {}
//! # extern crate windows_sys;
//! # extern crate widestring;
//! # #[cfg(all(windows, feature = "alloc"))]
//! # fn main() {
//! use windows_sys::{Win32::{System::Diagnostics::Debug::{FormatMessageW,
//!     FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_ALLOCATE_BUFFER,
//!     FORMAT_MESSAGE_IGNORE_INSERTS}, Foundation::{LocalFree, HLOCAL}}, core::PWSTR};
//! use std::ptr;
//! use widestring::U16CString;
//!
//! let error_code: u32 = 0;
//! let s: U16CString;
//! unsafe {
//!     // First, get a string buffer from some windows api such as FormatMessageW...
//!     let mut buffer: PWSTR = ptr::null_mut();
//!     FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM |
//!                    FORMAT_MESSAGE_ALLOCATE_BUFFER |
//!                    FORMAT_MESSAGE_IGNORE_INSERTS,
//!                    ptr::null(),
//!                    error_code, // error code from GetLastError()
//!                    0,
//!                    (&mut buffer as *mut PWSTR) as PWSTR,
//!                    0,
//!                    ptr::null_mut());
//!
//!     // Get the buffer as a wide string
//!     s = U16CString::from_ptr_str(buffer);
//!     // Since U16CString creates an owned copy, it's safe to free original buffer now
//!     // If you didn't want an owned copy, you could use &U16CStr.
//!     LocalFree(buffer as HLOCAL);
//! }
//! // Convert to a regular Rust String and use it to your heart's desire!
//! let message = s.to_string_lossy();
//! # assert_eq!(message, "The operation completed successfully.\r\n");
//! # }
//! ```
//!
//! [`OsString`]: std::ffi::OsString
//! [`OsStr`]: std::ffi::OsStr
//! [`CString`]: std::ffi::CString
//! [`CStr`]: std::ffi::CStr

#![warn(
    missing_docs,
    missing_debug_implementations,
    trivial_casts,
    trivial_numeric_casts,
    future_incompatible
)]
#![allow(renamed_and_removed_lints, stable_features)] // Until min version gets bumped
#![cfg_attr(not(feature = "std"), no_std)]
#![doc(html_root_url = "https://docs.rs/widestring/1.2.1")]
#![doc(test(attr(deny(warnings), allow(unused))))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(
    feature = "debugger_visualizer",
    feature(debugger_visualizer),
    debugger_visualizer(natvis_file = "../debug_metadata/widestring.natvis")
)]

#[cfg(feature = "alloc")]
extern crate alloc;

use crate::error::{DecodeUtf16Error, DecodeUtf32Error};
#[cfg(feature = "alloc")]
#[allow(unused_imports)]
use alloc::vec::Vec;
use core::fmt::Write;

pub mod error;
pub mod iter;
mod macros;
#[cfg(feature = "std")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
mod platform;
pub mod ucstr;
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub mod ucstring;
pub mod ustr;
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub mod ustring;
pub mod utfstr;
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub mod utfstring;

#[doc(hidden)]
pub use macros::internals;
pub use ucstr::{U16CStr, U32CStr, WideCStr};
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub use ucstring::{U16CString, U32CString, WideCString};
pub use ustr::{U16Str, U32Str, WideStr};
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub use ustring::{U16String, U32String, WideString};
pub use utfstr::{Utf16Str, Utf32Str, WideUtfStr};
#[cfg(feature = "alloc")]
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
pub use utfstring::{Utf16String, Utf32String, WideUtfString};

#[cfg(not(windows))]
/// Alias for [`u16`] or [`u32`] depending on platform. Intended to match typical C `wchar_t` size
/// on platform.
pub type WideChar = u32;

#[cfg(windows)]
/// Alias for [`u16`] or [`u32`] depending on platform. Intended to match typical C `wchar_t` size
/// on platform.
pub type WideChar = u16;

/// Creates an iterator over the UTF-16 encoded code points in `iter`, returning unpaired surrogates
/// as `Err`s.
///
/// # Examples
///
/// Basic usage:
///
/// ```
/// use std::char::decode_utf16;
///
/// // 𝄞mus<invalid>ic<invalid>
/// let v = [
///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
/// ];
///
/// assert_eq!(
///     decode_utf16(v.iter().cloned())
///         .map(|r| r.map_err(|e| e.unpaired_surrogate()))
///         .collect::<Vec<_>>(),
///     vec![
///         Ok('𝄞'),
///         Ok('m'), Ok('u'), Ok('s'),
///         Err(0xDD1E),
///         Ok('i'), Ok('c'),
///         Err(0xD834)
///     ]
/// );
/// ```
///
/// A lossy decoder can be obtained by replacing Err results with the replacement character:
///
/// ```
/// use std::char::{decode_utf16, REPLACEMENT_CHARACTER};
///
/// // 𝄞mus<invalid>ic<invalid>
/// let v = [
///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
/// ];
///
/// assert_eq!(
///     decode_utf16(v.iter().cloned())
///        .map(|r| r.unwrap_or(REPLACEMENT_CHARACTER))
///        .collect::<String>(),
///     "𝄞mus�ic�"
/// );
/// ```
#[must_use]
pub fn decode_utf16<I: IntoIterator<Item = u16>>(iter: I) -> iter::DecodeUtf16<I::IntoIter> {
    iter::DecodeUtf16::new(iter.into_iter())
}

/// Creates a lossy decoder iterator over the possibly ill-formed UTF-16 encoded code points in
/// `iter`.
///
/// This is equivalent to [`char::decode_utf16`][core::char::decode_utf16] except that any unpaired
/// UTF-16 surrogate values are replaced by
/// [`U+FFFD REPLACEMENT_CHARACTER`][core::char::REPLACEMENT_CHARACTER] (�) instead of returning
/// errors.
///
/// # Examples
///
/// ```
/// use widestring::decode_utf16_lossy;
///
/// // 𝄞mus<invalid>ic<invalid>
/// let v = [
///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0xDD1E, 0x0069, 0x0063, 0xD834,
/// ];
///
/// assert_eq!(
/// decode_utf16_lossy(v.iter().copied()).collect::<String>(),
/// "𝄞mus�ic�"
/// );
/// ```
#[inline]
#[must_use]
pub fn decode_utf16_lossy<I: IntoIterator<Item = u16>>(
    iter: I,
) -> iter::DecodeUtf16Lossy<I::IntoIter> {
    iter::DecodeUtf16Lossy {
        iter: decode_utf16(iter),
    }
}

/// Creates a decoder iterator over UTF-32 encoded code points in `iter`, returning invalid values
/// as `Err`s.
///
/// # Examples
///
/// ```
/// use widestring::decode_utf32;
///
/// // 𝄞mus<invalid>ic<invalid>
/// let v = [
///     0x1D11E, 0x6d, 0x75, 0x73, 0xDD1E, 0x69, 0x63, 0x23FD5A,
/// ];
///
/// assert_eq!(
///     decode_utf32(v.iter().copied())
///         .map(|r| r.map_err(|e| e.invalid_code_point()))
///         .collect::<Vec<_>>(),
///     vec![
///         Ok('𝄞'),
///         Ok('m'), Ok('u'), Ok('s'),
///         Err(0xDD1E),
///         Ok('i'), Ok('c'),
///         Err(0x23FD5A)
///     ]
/// );
/// ```
#[inline]
#[must_use]
pub fn decode_utf32<I: IntoIterator<Item = u32>>(iter: I) -> iter::DecodeUtf32<I::IntoIter> {
    iter::DecodeUtf32 {
        iter: iter.into_iter(),
    }
}

/// Creates a lossy decoder iterator over the possibly ill-formed UTF-32 encoded code points in
/// `iter`.
///
/// This is equivalent to [`decode_utf32`] except that any invalid UTF-32 values are replaced by
/// [`U+FFFD REPLACEMENT_CHARACTER`][core::char::REPLACEMENT_CHARACTER] (�) instead of returning
/// errors.
///
/// # Examples
///
/// ```
/// use widestring::decode_utf32_lossy;
///
/// // 𝄞mus<invalid>ic<invalid>
/// let v = [
///     0x1D11E, 0x6d, 0x75, 0x73, 0xDD1E, 0x69, 0x63, 0x23FD5A,
/// ];
///
/// assert_eq!(
/// decode_utf32_lossy(v.iter().copied()).collect::<String>(),
/// "𝄞mus�ic�"
/// );
/// ```
#[inline]
#[must_use]
pub fn decode_utf32_lossy<I: IntoIterator<Item = u32>>(
    iter: I,
) -> iter::DecodeUtf32Lossy<I::IntoIter> {
    iter::DecodeUtf32Lossy {
        iter: decode_utf32(iter),
    }
}

/// Creates an iterator that encodes an iterator over [`char`]s into UTF-8 bytes.
///
/// # Examples
///
/// ```
/// use widestring::encode_utf8;
///
/// let music = "𝄞music";
///
/// let encoded: Vec<u8> = encode_utf8(music.chars()).collect();
///
/// assert_eq!(encoded, music.as_bytes());
/// ```
#[must_use]
pub fn encode_utf8<I: IntoIterator<Item = char>>(iter: I) -> iter::EncodeUtf8<I::IntoIter> {
    iter::EncodeUtf8::new(iter.into_iter())
}

/// Creates an iterator that encodes an iterator over [`char`]s into UTF-16 [`u16`] code units.
///
/// # Examples
///
/// ```
/// use widestring::encode_utf16;
///
/// let encoded: Vec<u16> = encode_utf16("𝄞music".chars()).collect();
///
/// let v = [
///     0xD834, 0xDD1E, 0x006d, 0x0075, 0x0073, 0x0069, 0x0063,
/// ];
///
/// assert_eq!(encoded, v);
/// ```
#[must_use]
pub fn encode_utf16<I: IntoIterator<Item = char>>(iter: I) -> iter::EncodeUtf16<I::IntoIter> {
    iter::EncodeUtf16::new(iter.into_iter())
}

/// Creates an iterator that encodes an iterator over [`char`]s into UTF-32 [`u32`] values.
///
/// This iterator is a simple type cast from [`char`] to [`u32`], as any sequence of [`char`]s is
/// valid UTF-32.
///
/// # Examples
///
/// ```
/// use widestring::encode_utf32;
///
/// let encoded: Vec<u32> = encode_utf32("𝄞music".chars()).collect();
///
/// let v = [
///     0x1D11E, 0x006d, 0x0075, 0x0073, 0x0069, 0x0063,
/// ];
///
/// assert_eq!(encoded, v);
/// ```
#[must_use]
pub fn encode_utf32<I: IntoIterator<Item = char>>(iter: I) -> iter::EncodeUtf32<I::IntoIter> {
    iter::EncodeUtf32::new(iter.into_iter())
}

/// Debug implementation for any U16 string slice.
///
/// Properly encoded input data will output valid strings with escape sequences, however invalid
/// encoding will purposefully output any unpaired surrogate as \<XXXX> which is not a valid escape
/// sequence. This is intentional, as debug output is not meant to be parsed but read by humans.
fn debug_fmt_u16(s: &[u16], fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    debug_fmt_utf16_iter(decode_utf16(s.iter().copied()), fmt)
}

/// Debug implementation for any U16 string iterator.
///
/// Properly encoded input data will output valid strings with escape sequences, however invalid
/// encoding will purposefully output any unpaired surrogate as \<XXXX> which is not a valid escape
/// sequence. This is intentional, as debug output is not meant to be parsed but read by humans.
fn debug_fmt_utf16_iter(
    iter: impl Iterator<Item = Result<char, DecodeUtf16Error>>,
    fmt: &mut core::fmt::Formatter<'_>,
) -> core::fmt::Result {
    fmt.write_char('"')?;
    for res in iter {
        match res {
            Ok(ch) => {
                for c in ch.escape_debug() {
                    fmt.write_char(c)?;
                }
            }
            Err(e) => {
                write!(fmt, "\\<{:X}>", e.unpaired_surrogate())?;
            }
        }
    }
    fmt.write_char('"')
}

/// Debug implementation for any U16 string slice.
///
/// Properly encoded input data will output valid strings with escape sequences, however invalid
/// encoding will purposefully output any  invalid code point as \<XXXX> which is not a valid escape
/// sequence. This is intentional, as debug output is not meant to be parsed but read by humans.
fn debug_fmt_u32(s: &[u32], fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    debug_fmt_utf32_iter(decode_utf32(s.iter().copied()), fmt)
}

/// Debug implementation for any U16 string iterator.
///
/// Properly encoded input data will output valid strings with escape sequences, however invalid
/// encoding will purposefully output any  invalid code point as \<XXXX> which is not a valid escape
/// sequence. This is intentional, as debug output is not meant to be parsed but read by humans.
fn debug_fmt_utf32_iter(
    iter: impl Iterator<Item = Result<char, DecodeUtf32Error>>,
    fmt: &mut core::fmt::Formatter<'_>,
) -> core::fmt::Result {
    fmt.write_char('"')?;
    for res in iter {
        match res {
            Ok(ch) => {
                for c in ch.escape_debug() {
                    fmt.write_char(c)?;
                }
            }
            Err(e) => {
                write!(fmt, "\\<{:X}>", e.invalid_code_point())?;
            }
        }
    }
    fmt.write_char('"')
}

/// Debug implementation for any `char` iterator.
fn debug_fmt_char_iter(
    iter: impl Iterator<Item = char>,
    fmt: &mut core::fmt::Formatter<'_>,
) -> core::fmt::Result {
    fmt.write_char('"')?;
    iter.flat_map(|c| c.escape_debug())
        .try_for_each(|c| fmt.write_char(c))?;
    fmt.write_char('"')
}

/// Returns whether the code unit a UTF-16 surrogate value.
#[inline(always)]
#[allow(dead_code)]
const fn is_utf16_surrogate(u: u16) -> bool {
    u >= 0xD800 && u <= 0xDFFF
}

/// Returns whether the code unit a UTF-16 high surrogate value.
#[inline(always)]
#[allow(dead_code)]
const fn is_utf16_high_surrogate(u: u16) -> bool {
    u >= 0xD800 && u <= 0xDBFF
}

/// Returns whether the code unit a UTF-16 low surrogate value.
#[inline(always)]
const fn is_utf16_low_surrogate(u: u16) -> bool {
    u >= 0xDC00 && u <= 0xDFFF
}

/// Convert a UTF-16 surrogate pair to a `char`. Does not validate if the surrogates are valid.
#[inline(always)]
unsafe fn decode_utf16_surrogate_pair(high: u16, low: u16) -> char {
    let c: u32 = ((((high - 0xD800) as u32) << 10) | ((low) - 0xDC00) as u32) + 0x1_0000;
    // SAFETY: we checked that it's a legal unicode value
    core::char::from_u32_unchecked(c)
}

/// Validates whether a slice of 16-bit values is valid UTF-16, returning an error if it is not.
#[inline(always)]
fn validate_utf16(s: &[u16]) -> Result<(), crate::error::Utf16Error> {
    for (index, result) in crate::decode_utf16(s.iter().copied()).enumerate() {
        if let Err(e) = result {
            return Err(crate::error::Utf16Error::empty(index, e));
        }
    }
    Ok(())
}

/// Validates whether a vector of 16-bit values is valid UTF-16, returning an error if it is not.
#[inline(always)]
#[cfg(feature = "alloc")]
fn validate_utf16_vec(v: Vec<u16>) -> Result<Vec<u16>, crate::error::Utf16Error> {
    for (index, result) in crate::decode_utf16(v.iter().copied()).enumerate() {
        if let Err(e) = result {
            return Err(crate::error::Utf16Error::new(v, index, e));
        }
    }
    Ok(v)
}

/// Validates whether a slice of 32-bit values is valid UTF-32, returning an error if it is not.
#[inline(always)]
fn validate_utf32(s: &[u32]) -> Result<(), crate::error::Utf32Error> {
    for (index, result) in crate::decode_utf32(s.iter().copied()).enumerate() {
        if let Err(e) = result {
            return Err(crate::error::Utf32Error::empty(index, e));
        }
    }
    Ok(())
}

/// Validates whether a vector of 32-bit values is valid UTF-32, returning an error if it is not.
#[inline(always)]
#[cfg(feature = "alloc")]
fn validate_utf32_vec(v: Vec<u32>) -> Result<Vec<u32>, crate::error::Utf32Error> {
    for (index, result) in crate::decode_utf32(v.iter().copied()).enumerate() {
        if let Err(e) = result {
            return Err(crate::error::Utf32Error::new(v, index, e));
        }
    }
    Ok(v)
}

/// Copy of unstable core::slice::range to soundly handle ranges
/// TODO: Replace with core::slice::range when it is stabilized
#[track_caller]
#[allow(dead_code, clippy::redundant_closure)]
fn range<R>(range: R, bounds: core::ops::RangeTo<usize>) -> core::ops::Range<usize>
where
    R: core::ops::RangeBounds<usize>,
{
    #[inline(never)]
    #[cold]
    #[track_caller]
    #[allow(unknown_lints, clippy::uninlined_format_args)]
    fn slice_end_index_len_fail(index: usize, len: usize) -> ! {
        panic!(
            "range end index {} out of range for slice of length {}",
            index, len
        );
    }

    #[inline(never)]
    #[cold]
    #[track_caller]
    #[allow(unknown_lints, clippy::uninlined_format_args)]
    fn slice_index_order_fail(index: usize, end: usize) -> ! {
        panic!("slice index starts at {} but ends at {}", index, end);
    }

    #[inline(never)]
    #[cold]
    #[track_caller]
    fn slice_start_index_overflow_fail() -> ! {
        panic!("attempted to index slice from after maximum usize");
    }

    #[inline(never)]
    #[cold]
    #[track_caller]
    fn slice_end_index_overflow_fail() -> ! {
        panic!("attempted to index slice up to maximum usize");
    }

    use core::ops::Bound::*;

    let len = bounds.end;

    let start = range.start_bound();
    let start = match start {
        Included(&start) => start,
        Excluded(start) => start
            .checked_add(1)
            .unwrap_or_else(|| slice_start_index_overflow_fail()),
        Unbounded => 0,
    };

    let end = range.end_bound();
    let end = match end {
        Included(end) => end
            .checked_add(1)
            .unwrap_or_else(|| slice_end_index_overflow_fail()),
        Excluded(&end) => end,
        Unbounded => len,
    };

    if start > end {
        slice_index_order_fail(start, end);
    }
    if end > len {
        slice_end_index_len_fail(end, len);
    }

    core::ops::Range { start, end }
}

/// Similar to core::slice::range, but returns [`None`] instead of panicking.
fn range_check<R>(range: R, bounds: core::ops::RangeTo<usize>) -> Option<core::ops::Range<usize>>
where
    R: core::ops::RangeBounds<usize>,
{
    use core::ops::Bound::*;

    let len = bounds.end;

    let start = range.start_bound();
    let start = match start {
        Included(&start) => start,
        Excluded(start) => start.checked_add(1)?,
        Unbounded => 0,
    };

    let end = range.end_bound();
    let end = match end {
        Included(end) => end.checked_add(1)?,
        Excluded(&end) => end,
        Unbounded => len,
    };

    if start > end || end > len {
        return None;
    }
    Some(core::ops::Range { start, end })
}
