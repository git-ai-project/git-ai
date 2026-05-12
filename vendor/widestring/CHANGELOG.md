# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.1] - 2025-10-08 <a name="1.2.1"></a>
### Changed
- Git repository URL has changed due to GitHub user name change. Old URL is redirected.

### Fixed
- `trim_end` character boundary panic when processing 4-byte characters. By [@syrflover].
- Fixed completely broken `U16String::insert_char` which always panicked. Fixes [#46].
- Documentation typos on `WideCString` type alias.
- Fixed `unused_import` lint on `wide*` macros.

### Added
- Added GDB pretty printers for `Utf32String` and `Utf32Str`. You can load them from `gdb/widestring.py`.

## [1.2.0] - 2024-03-15 <a name="1.2.0"></a>
### Changed
- Minor restructing of included license file locations to be more consistent with crates ecosystem.
- Moved to `windows-sys` for documentation tests instead of legacy `winapi` crate. Fixes [#40].

### Added
- Added `.lines()` method on various string slices. Fixes [#33]. By [@mr-adult].

### Fixed
- Removed panic in `Utf16String::truncate` when `new_len` greater than string length. Fixes [#39].
- Fixed various clippy warnings.
- Fixed debugger visualizer for Rust 1.82+ due to changes in `core` library structures.

## [1.1.0] - 2022-04-06 <a name="1.1.0"></a>
### Added
- `Utf32String::into_char_vec` and missing conversion to `Vec<char>` for `Utf32String`. Fixes [#37].
- `include_utf16str!` macro to include UTF-16 file at compile-time as `Utf16Str`. By [@daxpedda].

### Fixed
- `U16String::pop_char` panics with surrogate string. Fixes [#38].
- Various import warnings and new clippy warnings, plus stabilized debugger visualizer warnings.

## [1.0.2] - 2022-07-15 <a name="1.0.2"></a>
### Fixed
- Correctly check for and error on nul values in C-string macros `u16cstr!`, `u32cstr!`, and
  `widecstr!`. Fixes [#28].

## [1.0.1] - 2022-06-24 <a name="1.0.1"></a>
### Fixed
- Reduce collision potential for macros. By [@OpenByteDev].

## [1.0.0] - 2022-06-21 <a name="1.0.0"></a>
### Changed
- **Breaking Change** Minimum supported Rust version is now 1.58.
- Added `#[must_use]` attributes to many crate functions, as appropriate.
- Remove `unsafe` qualifiers from `as_mut_ptr` and `as_mut_ptr_range` to match standard library. By
  [@yescallop].

### Added
- Added `new` function that creates and empty string to `U16CString` and `U32CString` to match other
  string types.
- Additional `From` implementations for conversion to `OsString`.

## [1.0.0-beta.1] - 2021-11-08 <a name="1.0.0-beta.1"></a>
### Changed
- **Breaking Change** Minimum supported Rust version is now 1.56.
- **Breaking Change** The following methods on `U16String` and `U32String` have been renamed and
  replaced by functions with different semantics:
  - `pop` is now `pop_char`
  - `remove` is now `remove_char`
  - `insert` is now `insert_char`
- **Breaking Change** Moved and renamed the following iterator types:
  - `iter::Utf16Chars` renamed to `CharsUtf16` and moved to `ustr` and `ucstr`
  - `iter::Utf32Chars` renamed to `CharsUtf32` and moved to `ustr` and `ucstr`
  - `iter::CharsLossy` split and renamed to `CharsLossyUtf16` and `CharsLossyUtf32` and moved to
    `ustr` and `ucstr`
  - `iter::Utf16CharIndices` renamed to `CharIndicesUtf16` and moved to `ustr` and `ucstr`
  - `iter::Utf16CharIndicesLossy` renamed to `CharIndicesLossyUtf16` and moved to `ustr` and `ucstr`
- **Breaking Change** `error::FromUtf16Error` and `error::FromUtf32Error` has been renamed to
  `Utf16Error` and `Utf32Error` respectively and expanded with more details about the error.
- Migrated crate to Rust 2021 edition.
- The following methods on `U16Str` and `U32Str` are now `const`:
  - `from_slice`
  - `as_slice`
  - `as_ptr`
  - `len`
  - `is_empty`
- The following methods on `U16CStr` and `U32CStr` are now `const`:
  - `from_slice_unchecked`
  - `as_slice_with_nul`
  - `as_ptr`
  - `len`
  - `is_empty`
- The following methods on `U16String` and `U32String` are now `const`:
  - `new`

### Added
- Added new UTF-encoded string types and associated types:
  - `Utf16Str`
  - `Utf32Str`
  - `Utf16String`
  - `Utf32String`
- Added macros to convert string literals into `const` wide string slices:
  - `u16str!`
  - `u16cstr!`
  - `u32str!`
  - `u32cstr!`
  - `widestr!`
  - `widecstr!`
  - `utf16str!`
  - `utf32str!`
- Added `NUL_TERMINATOR` associated constant to `U16CStr`, `U32CStr`, `U16CString`, and 
  `U32CString`.
- Added `DoubleEndedIterator` and `ExactSizeIterator` implementations to a number of iterator types.
- Added new UTF encoding functions alongside existing decode functions:
  - `encode_utf8`
  - `encode_utf16`
  - `encode_utf32`
- Added various methods:
  - `repeat` on `U16Str`, `U32Str`, `U16CStr`, and `U32CStr`
  - `shrink_to` on `U16String` and `U32String`
  - `retain` on `U16String` and `U32String`
  - `drain` on `U16String` and `U32String`
  - `replace_range` on `U16String` and `U32String`
  - `get`, `get_mut`, `get_unchecked`, and `get_unchecked_mut` on `U16CStr` and `U32CStr`
  - `split_at` and `split_at_mut` on `U16CStr` and `U32CStr`
- Added more trait implementations.

### Removed
- **Breaking Change** Functions and types deprecated in 0.5 have been removed.
- **Breaking Change** The following types and traits, which were implementation details, have been
  removed. Use the existing non-generic types instead (e.g. use `U16Str` instead of `UStr<u16>`).
  - `UChar`
  - `UStr`
  - `UCStr`
  - `UString`
  - `UCString`
- **Breaking Change** Removed `IndexMut<RangeFull>` trait implementation of `U16CString` and
  `U32CString`. Use the unsafe `get_mut` method instead, which also supports more ranges.

### Fixed
- **Breaking Change** The iterator returned by `U16Str::char_indices` and `U16CStr::char_indices`
  is now over `(usize, Result<char, DecodeUtf16Error>)` tuples instead of the reverse order, to
  better match standard library string iterators. The same is true of `U16Str::char_indices_lossy`
  and `U16CStr::char_indices_lossy`. This matches what was stated in original documentation.
- `U32Str::to_string` and `U32CStr::to_string` now only allocate once instead of twice.

## [0.5.1] - 2021-10-23 <a name="0.5.1"></a>
### Fixed
- Fixed a regression in 0.5.0 where zero-length vectors and strings were incorrectly causing panics
  in `UCString::from_vec` and `UCString::from_str`. Fixes [#22].
- Modified an implentation detail in `ustr::to_string` & `ustr::to_string_lossy` to remove possibly
  unsafe behaviour.
  
## [0.5.0] - 2021-10-12 <a name="0.5.0"></a>
### Changed
- **Breaking Change** Minimum supported Rust version is now 1.48.
- **Breaking Change** Renamed a number of types and functions to increase consistency and clarity.
  This also meant renaming errors to more clearly convey error and trying to be more consistent with
  name conventions and functionality across types. Check renamed function docs for any changes in
  functionality, as there have been some minor tweaks (mostly relaxing/removing error conditions and
  reducing panics). Old names have been deprecated to ease transition and will be removed in a
  future release. Fixes [#18].
  - `MissingNulError` => `error::MissingNulTerminator`
  - `FromUtf32Error` => `error::FromUtf32Error`
  - `NulError` => `error::ContainsNul`
  - `UCStr::from_ptr_with_nul` => `from_ptr_unchecked`
  - `UCStr::from_slice_with_nul` => `from_slice_truncate`
  - `UCStr::from_slice_with_nul_unchecked` => `from_slice_unchecked`
  - `U32CStr::from_char_ptr_with_nul` => `from_char_ptr_unchecked`
  - `U32CStr::from_char_slice_with_nul` => `from_char_slice_truncate`
  - `U32CStr::from_char_slice_with_nul_unchecked` => `from_char_slice_unchecked`
  - `UCString::new` => `from_vec`
  - `UCString::from_vec_with_nul` => `from_vec_truncate`
  - `UCString::from_ustr_with_nul` => `from_ustr_truncate`
  - `UCString::from_ptr_with_nul` => `from_ptr_truncate`
  - `UCString::from_str_with_nul` => `from_str_truncate`
  - `UCString::from_os_str_with_nul` => `from_os_str_truncate`
  - `U32CString::from_chars_with_nul` => `from_chars_truncate`
  - `U32CString::from_char_ptr_with_nul` => `from_char_ptr_truncate`
- Improved implementations in some areas to reduce unncessary double allocations.
- Improved `Debug` implementations. No more debugging lists of raw integer values.
- Migrated crate to Rust 2018 edition.
- Made crate package [REUSE compliant](https://reuse.software/).
- Improved documentation and used intra-doc links.

### Added
- Added crate-level functions `decode_utf16`, `decode_utf16_lossy`, `decode_utf32`, and
  `decode_utf32_lossy` and associated iterators. Note that `decode_utf16` is an alias of
  `core::char::decode_utf16`, but provided for consistency.
- Added `display` method to to both `UStr` and `UCStr` to display strings in formatting without heap
  allocations, similar to `Path::display`. Fixes [#20].
- Added more trait implementations, including more index operations and string formatting via
  `Write` trait. Fixes [#19].
- Added new functions:
    - `UStr::from_ptr_mut`
    - `UStr::from_slice_mut`
    - `UStr::as_mut_slice`
    - `UStr::as_mut_ptr`
    - `UStr::as_ptr_range`
    - `UStr::as_mut_ptr_range`
    - `UStr::get`
    - `UStr::get_mut`
    - `UStr::get_unchecked`
    - `UStr::get_unchecked_mut`
    - `UStr::split_at`
    - `UStr::split_at_mut`
    - `UStr::chars`
    - `UStr::chars_lossy`
    - `U16Str::char_indices`
    - `U16Str::char_indices_lossy`
    - `U32Str::from_char_ptr_mut`
    - `U32Str::from_char_slice_mut`
    - `UCStr::from_ptr`
    - `UCStr::from_ptr_truncate`
    - `UCStr::from_slice`
    - `UCStr::as_ustr`
    - `UCStr::from_ptr_str_mut`
    - `UCStr::from_ptr_mut`
    - `UCStr::from_ptr_truncate_mut`
    - `UCStr::from_ptr_unchecked_mut`
    - `UCStr::from_slice_mut`
    - `UCStr::from_slice_truncate_mut`
    - `UCStr::from_slice_unchecked_mut`
    - `UCStr::as_mut_slice`
    - `UCStr::as_mut_ptr`
    - `UCStr::as_ustr_with_nul`
    - `UCStr::as_mut_ustr`
    - `UCStr::as_ptr_range`
    - `UCStr::as_mut_ptr_range`
    - `UCStr::chars`
    - `UCStr::chars_lossy`
    - `U16CStr::char_indices`
    - `U16CStr::char_indices_lossy`
    - `U32CStr::from_char_ptr_str_mut`
    - `U32CStr::from_char_ptr_mut`
    - `U32CStr::from_char_ptr_truncate_mut`
    - `U32CStr::from_char_ptr_unchecked_mut`
    - `U32CStr::from_char_slice_mut`
    - `U32CStr::from_char_slice_truncate_mut`
    - `U32CStr::from_char_slice_unchecked_mut`
    - `U32CStr::from_char_ptr`
    - `U32CStr::from_char_ptr_truncate`
    - `U32CStr::from_char_slice`
    - `UString::as_vec`
    - `UString::as_mut_vec`
    - `UString::push_char`
    - `UString::truncate`
    - `UString::pop`
    - `UString::remove`
    - `UString::insert`
    - `UString::insert_ustr`
    - `UString::split_off`
    - `UCString::as_mut_ucstr`
    - `UCString::into_ustring`
    - `UCString::into_ustring_with_nul`
    - `U32CString::from_char_ptr_str`

### Deprecated
- Deprecated functions as part of simplifying to increase clarity. These will be removed entirely
  in a future release.
    - `MissingNulError`. Use `error::MissingNulTerminator` instead.
    - `FromUtf32Error`. Use `error::FromUtf32Error` instead.
    - `NulError`. Use `error::ContainsNul` instead.
    - `UCStr::from_ptr_with_nul`. Use `from_ptr_unchecked` instead.
    - `UCStr::from_slice_with_nul`. Use `from_slice_truncate` instead.
    - `UCStr::from_slice_with_nul_unchecked`. Use `from_slice_unchecked` instead.
    - `U32CStr::from_char_ptr_with_nul`. Use `from_char_ptr_unchecked` instead.
    - `U32CStr::from_char_slice_with_nul`. Use `from_char_slice_truncate` instead.
    - `U32CStr::from_char_slice_with_nul_unchecked`. Use `from_char_slice_unchecked` instead.
    - `UCString::new`. Use `from_vec` instead.
    - `UCString::from_vec_with_nul_unchecked`. Use `from_vec_unchecked` instead.
    - `UCString::from_ustr_with_nul_unchecked`. Use `from_ustr_unchecked` instead.
    - `UCString::from_ptr_with_nul_unchecked`. Use `from_ptr_unchecked` instead.
    - `UCString::from_str_with_nul_unchecked`. Use `from_str_unchecked` instead.
    - `UCString::from_os_str_with_nul_unchecked`. Use `from_os_str_unchecked` instead.
    - `UCString::from_vec_with_nul`. Use `from_vec_truncate` instead.
    - `UCString::from_ustr_with_nul`. Use `from_ustr_truncate` instead.
    - `UCString::from_ptr_with_nul`. Use `from_ptr_truncate` instead.
    - `UCString::from_str_with_nul`. Use `from_str_truncate` instead.
    - `UCString::from_os_str_with_nul`. Use `from_os_str_truncate` instead.
    - `U32CString::from_chars_with_nul_unchecked`. Use `from_chars_unchecked` instead.
    - `U32CString::from_char_ptr_with_nul_unchecked`. Use `from_char_ptr_unchecked` instead.
    - `U32CString::from_chars_with_nul`. Use `from_chars_truncate` instead.
    - `U32CString::from_char_ptr_with_nul`. Use `from_char_ptr_truncate` instead.
- Deprecated error types in the crate root. Use the errors directly from `error` module instead.

## [0.4.3] - 2020-10-05 <a name="0.4.3"></a>
### Fixed
- Fixed undefined behaviours and cleaned up clippy warnings. By [@joshwd36].

## [0.4.2] - 2020-06-09 <a name="0.4.2"></a>
### Fixed
- Fixed compile errors on pre-1.36.0 Rust due to unstable `alloc` crate. Minimum supported version
  is Rust 1.34.2, the rust version for Debian stable. Fixes [#14].

## [0.4.1] - 2020-06-08 <a name="0.4.1"></a>
### ***Yanked***
### Changed
- Now supports `no_std`. Added the `std` and `alloc` features, enabled by default. `U16String`,
  `U32String`, `U16CString`, and `U32CString` and their aliases all require the `alloc` or `std`
  feature. By [@nicbn].

## [0.4.0] - 2018-08-18 <a name="0.4.0"></a>
### Added
- New `U32String`, `U32Str`, `U32CString`, and `U32CStr` types for dealing with UTF-32 FFI. These
  new types are roughly equivalent to the existing UTF-16 types.
- `WideChar` is a type alias to `u16` on Windows but `u32` on non-Windows platforms.
- The generic types `UString`, `UStr`, `UCString` and `UCStr` are used to implement the string
  types.

### Changed
- **Breaking Change** Existing wide string types have been renamed to `U16String`, `U16Str`,
  `U16CString`, and `U16CStr` (previously `WideString`, `WideStr`, etc.). Some function have
  also been renamed to reflect this change (`wide_str` to `u16_str`, etc.).
- **Breaking Change** `WideString`, `WideStr`, `WideCString`, and `WideCStr` are now type aliases
  that vary between platforms. On Windows, these are aliases to the `U16` types and are equivalent
  to the previous version, but on non-Windows platforms these alias the new `U32` types instead.
  See crate documentation for more details.

## [0.3.0] - 2018-03-17 <a name="0.3.0"></a>
### Added
- Additional unchecked functions on `WideCString`.
- All types now implement `Default`.
- `WideString::shrink_to_fit`
- `WideString::into_boxed_wide_str` and `Box<WideStr>::into_wide_string`.
- `WideCString::into_boxed_wide_c_str` and `Box<WideCStr>::into_wide_c_string`.
- `From` and `Default` implementations for boxed `WideStr` and boxed `WideCStr`.

### Changed
- Renamed `WideCString::from_vec` to replace `WideCString::new`. To create empty string, use
  `WideCString::default()` now.
- `WideCString` now implements `Drop`, which sets the string to an empty string to prevent invalid
  unsafe code from working correctly when it should otherwise break. Also see `Drop` implementation
  of `CString`.
- Writing changelog manually.
- Upgraded winapi dev dependency.
- Now requires at least Rust 1.17+ to compile (previously, was Rust 1.8).

## [0.2.2] - 2016-09-09 <a name="0.2.2"></a>
### Fixed
- Make `WideCString::into_raw` correctly forget the original self.

## [0.2.1] - 2016-08-12 <a name="0.2.1"></a>
### Added
- `into_raw`/`from_raw` on `WideCString`. Closes [#2].

## [0.2.0] - 2016-05-31 <a name="0.2.0"></a>
### Added
- `Default` trait to wide strings.
- Traits for conversion of strings to `Cow`.
### Changed
- Methods & traits to bring to parity with Rust 1.9 string APIs.

## 0.1.0 - 2016-02-06 <a name="0.1.0"></a>
### Added
- Initial release.

[#2]: https://github.com/VoidStarKat/widestring-rs/issues/2
[#14]: https://github.com/VoidStarKat/widestring-rs/issues/14
[#18]: https://github.com/VoidStarKat/widestring-rs/issues/18
[#19]: https://github.com/VoidStarKat/widestring-rs/issues/19
[#20]: https://github.com/VoidStarKat/widestring-rs/issues/20
[#22]: https://github.com/VoidStarKat/widestring-rs/issues/22
[#28]: https://github.com/VoidStarKat/widestring-rs/issues/28
[#37]: https://github.com/VoidStarKat/widestring-rs/issues/37
[#38]: https://github.com/VoidStarKat/widestring-rs/issues/38
[#33]: https://github.com/VoidStarKat/widestring-rs/issues/33
[#39]: https://github.com/VoidStarKat/widestring-rs/issues/39
[#40]: https://github.com/VoidStarKat/widestring-rs/issues/40
[#46]: https://github.com/VoidStarKat/widestring-rs/issues/46

[@nicbn]: https://github.com/nicbn
[@joshwd36]: https://github.com/joshwd36
[@yescallop]: https://github.com/yescallop
[@OpenByteDev]: https://github.com/OpenByteDev
[@daxpedda]: https://github.com/daxpedda
[@mr-adult]: https://github.com/mr-adult
[@syrflover]: https://github.com/syrflover

[Unreleased]: https://github.com/VoidStarKat/widestring-rs/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/VoidStarKat/widestring-rs/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/VoidStarKat/widestring-rs/compare/v1.0.2...v1.1.0
[1.0.2]: https://github.com/VoidStarKat/widestring-rs/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/VoidStarKat/widestring-rs/compare/v1.0.0...v1.0.1
[1.0.0]: https://github.com/VoidStarKat/widestring-rs/compare/v1.0.0-beta.1...v1.0.0
[1.0.0-beta.1]: https://github.com/VoidStarKat/widestring-rs/compare/v0.5.1...v1.0.0-beta.1
[0.5.1]: https://github.com/VoidStarKat/widestring-rs/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/VoidStarKat/widestring-rs/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/VoidStarKat/widestring-rs/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/VoidStarKat/widestring-rs/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/VoidStarKat/widestring-rs/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/VoidStarKat/widestring-rs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/VoidStarKat/widestring-rs/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/VoidStarKat/widestring-rs/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/VoidStarKat/widestring-rs/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/VoidStarKat/widestring-rs/compare/v0.1.0...v0.2.0
