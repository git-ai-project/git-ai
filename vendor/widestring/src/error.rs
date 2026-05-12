//! Errors returned by functions in this crate.

#[cfg(feature = "alloc")]
#[allow(unused_imports)]
use alloc::vec::Vec;

/// An error returned to indicate a problem with nul values occurred.
///
/// The error will either being a [`MissingNulTerminator`] or [`ContainsNul`].
/// The error optionally returns the ownership of the invalid vector whenever a vector was owned.
#[derive(Debug, Clone)]
pub enum NulError<C> {
    /// A terminating nul value was missing.
    MissingNulTerminator(MissingNulTerminator),
    /// An interior nul value was found.
    ContainsNul(ContainsNul<C>),
}

impl<C> NulError<C> {
    /// Consumes this error, returning the underlying vector of values which generated the error in
    /// the first place.
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn into_vec(self) -> Option<Vec<C>> {
        match self {
            Self::MissingNulTerminator(_) => None,
            Self::ContainsNul(e) => e.into_vec(),
        }
    }
}

impl<C> core::fmt::Display for NulError<C> {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::MissingNulTerminator(e) => e.fmt(f),
            Self::ContainsNul(e) => e.fmt(f),
        }
    }
}

#[cfg(feature = "std")]
impl<C> std::error::Error for NulError<C>
where
    C: core::fmt::Debug + 'static,
{
    #[inline]
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MissingNulTerminator(e) => Some(e),
            Self::ContainsNul(e) => Some(e),
        }
    }
}

impl<C> From<MissingNulTerminator> for NulError<C> {
    #[inline]
    fn from(value: MissingNulTerminator) -> Self {
        Self::MissingNulTerminator(value)
    }
}

impl<C> From<ContainsNul<C>> for NulError<C> {
    #[inline]
    fn from(value: ContainsNul<C>) -> Self {
        Self::ContainsNul(value)
    }
}

/// An error returned from to indicate that a terminating nul value was missing.
#[derive(Debug, Clone)]
pub struct MissingNulTerminator {
    _unused: (),
}

impl MissingNulTerminator {
    pub(crate) fn new() -> Self {
        Self { _unused: () }
    }
}

impl core::fmt::Display for MissingNulTerminator {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "missing terminating nul value")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for MissingNulTerminator {}

/// An error returned to indicate that an invalid nul value was found in a string.
///
/// The error indicates the position in the vector where the nul value was found, as well as
/// returning the ownership of the invalid vector.
#[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
#[derive(Debug, Clone)]
pub struct ContainsNul<C> {
    index: usize,
    #[cfg(feature = "alloc")]
    pub(crate) inner: Option<Vec<C>>,
    #[cfg(not(feature = "alloc"))]
    _p: core::marker::PhantomData<C>,
}

impl<C> ContainsNul<C> {
    #[cfg(feature = "alloc")]
    pub(crate) fn new(index: usize, v: Vec<C>) -> Self {
        Self {
            index,
            inner: Some(v),
        }
    }

    #[cfg(feature = "alloc")]
    pub(crate) fn empty(index: usize) -> Self {
        Self { index, inner: None }
    }

    #[cfg(not(feature = "alloc"))]
    pub(crate) fn empty(index: usize) -> Self {
        Self {
            index,
            _p: core::marker::PhantomData,
        }
    }

    /// Returns the index of the invalid nul value in the slice.
    #[inline]
    #[must_use]
    pub fn nul_position(&self) -> usize {
        self.index
    }

    /// Consumes this error, returning the underlying vector of values which generated the error in
    /// the first place.
    ///
    /// If the sequence that generated the error was a reference to a slice instead of a [`Vec`],
    /// this will return [`None`].
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn into_vec(self) -> Option<Vec<C>> {
        self.inner
    }
}

impl<C> core::fmt::Display for ContainsNul<C> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "invalid nul value found at position {}", self.index)
    }
}

#[cfg(feature = "std")]
impl<C> std::error::Error for ContainsNul<C> where C: core::fmt::Debug {}

/// An error that can be returned when decoding UTF-16 code points.
///
/// This struct is created when using the [`DecodeUtf16`][crate::iter::DecodeUtf16] iterator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeUtf16Error {
    unpaired_surrogate: u16,
}

impl DecodeUtf16Error {
    pub(crate) fn new(unpaired_surrogate: u16) -> Self {
        Self { unpaired_surrogate }
    }

    /// Returns the unpaired surrogate which caused this error.
    #[must_use]
    pub fn unpaired_surrogate(&self) -> u16 {
        self.unpaired_surrogate
    }
}

impl core::fmt::Display for DecodeUtf16Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "unpaired surrogate found: {:x}", self.unpaired_surrogate)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecodeUtf16Error {}

/// An error that can be returned when decoding UTF-32 code points.
///
/// This error occurs when a [`u32`] value is outside the 21-bit Unicode code point range
/// (>`U+10FFFF`) or is a UTF-16 surrogate value.
#[derive(Debug, Clone)]
pub struct DecodeUtf32Error {
    code: u32,
}

impl DecodeUtf32Error {
    pub(crate) fn new(code: u32) -> Self {
        Self { code }
    }

    /// Returns the invalid code point value which caused the error.
    #[must_use]
    pub fn invalid_code_point(&self) -> u32 {
        self.code
    }
}

impl core::fmt::Display for DecodeUtf32Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid UTF-32 code point: {:x}", self.code)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for DecodeUtf32Error {}

/// Errors which can occur when attempting to interpret a sequence of `u16` as UTF-16.
#[derive(Debug, Clone)]
pub struct Utf16Error {
    index: usize,
    source: DecodeUtf16Error,
    #[cfg(feature = "alloc")]
    inner: Option<Vec<u16>>,
}

impl Utf16Error {
    #[cfg(feature = "alloc")]
    pub(crate) fn new(inner: Vec<u16>, index: usize, source: DecodeUtf16Error) -> Self {
        Self {
            inner: Some(inner),
            index,
            source,
        }
    }

    #[cfg(feature = "alloc")]
    pub(crate) fn empty(index: usize, source: DecodeUtf16Error) -> Self {
        Self {
            index,
            source,
            inner: None,
        }
    }

    #[cfg(not(feature = "alloc"))]
    pub(crate) fn empty(index: usize, source: DecodeUtf16Error) -> Self {
        Self { index, source }
    }

    /// Returns the index in the given string at which the invalid UTF-16 value occurred.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Consumes this error, returning the underlying vector of values which generated the error in
    /// the first place.
    ///
    /// If the sequence that generated the error was a reference to a slice instead of a [`Vec`],
    /// this will return [`None`].
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn into_vec(self) -> Option<Vec<u16>> {
        self.inner
    }
}

impl core::fmt::Display for Utf16Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "unpaired UTF-16 surrogate {:x} at index {}",
            self.source.unpaired_surrogate(),
            self.index
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Utf16Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Errors which can occur when attempting to interpret a sequence of `u32` as UTF-32.
#[derive(Debug, Clone)]
pub struct Utf32Error {
    index: usize,
    source: DecodeUtf32Error,
    #[cfg(feature = "alloc")]
    inner: Option<Vec<u32>>,
}

impl Utf32Error {
    #[cfg(feature = "alloc")]
    pub(crate) fn new(inner: Vec<u32>, index: usize, source: DecodeUtf32Error) -> Self {
        Self {
            inner: Some(inner),
            index,
            source,
        }
    }

    #[cfg(feature = "alloc")]
    pub(crate) fn empty(index: usize, source: DecodeUtf32Error) -> Self {
        Self {
            index,
            source,
            inner: None,
        }
    }

    #[cfg(not(feature = "alloc"))]
    pub(crate) fn empty(index: usize, source: DecodeUtf32Error) -> Self {
        Self { index, source }
    }

    /// Returns the index in the given string at which the invalid UTF-32 value occurred.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Consumes this error, returning the underlying vector of values which generated the error in
    /// the first place.
    ///
    /// If the sequence that generated the error was a reference to a slice instead of a [`Vec`],
    /// this will return [`None`].
    #[inline]
    #[cfg(feature = "alloc")]
    #[cfg_attr(docsrs, doc(cfg(feature = "alloc")))]
    #[must_use]
    pub fn into_vec(self) -> Option<Vec<u32>> {
        self.inner
    }
}

impl core::fmt::Display for Utf32Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "invalid UTF-32 value {:x} at index {}",
            self.source.invalid_code_point(),
            self.index
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Utf32Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
