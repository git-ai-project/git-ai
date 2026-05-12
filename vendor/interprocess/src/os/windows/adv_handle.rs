use {
    super::{c_wrappers, winprelude::*},
    crate::TryClone,
    std::{
        fmt::{self, Debug, Formatter},
        io,
        mem::ManuallyDrop,
        num::NonZeroUsize,
    },
};

/// Like [`OwnedHandle`], but with low-bit tagging and a zero niche.
///
/// The boolean generic parameters correspond to whether the tag bits are enabled or not.
#[repr(transparent)]
pub struct AdvOwnedHandle<const TAG0: bool = false, const TAG1: bool = false>(NonZeroUsize);

impl<const TAG0: bool, const TAG1: bool> Drop for AdvOwnedHandle<TAG0, TAG1> {
    #[inline]
    fn drop(&mut self) { unsafe { OwnedHandle::from_raw_handle(self.as_raw_handle()) }; }
}

/// Private utilities.
impl<const TAG0: bool, const TAG1: bool> AdvOwnedHandle<TAG0, TAG1> {
    #[inline(always)]
    const fn mk_tag(tag0: bool, tag1: bool) -> usize {
        (((TAG1 && tag1) as usize) << 1) | ((TAG0 && tag0) as usize)
    }
    #[inline(always)]
    const fn usize_with_tag(val: usize, tag0: bool, tag1: bool) -> usize {
        val | Self::mk_tag(tag0, tag1)
    }
    const TAG_MASK: usize = Self::mk_tag(true, true);
    const TAG_UNMASK: usize = !Self::TAG_MASK;

    /// Ignores tags that are not stored.
    #[inline]
    fn new(h: OwnedHandle, tag0: bool, tag1: bool) -> Self {
        // SAFETY: valid handles (as guaranteed by OwnedHandle) are never zero
        Self(unsafe {
            NonZeroUsize::new_unchecked(h.into_raw_handle() as usize | Self::mk_tag(tag0, tag1))
        })
    }
    #[inline(always)]
    const fn tag0_or_false(&self) -> bool { self.0.get() & 1 != 0 }
    #[inline(always)]
    const fn tag1_or_false(&self) -> bool { self.0.get() & 2 != 0 }
}

impl<const TAG1: bool> AdvOwnedHandle<true, TAG1> {
    /// Returns the value of tag bit 0.
    #[inline(always)]
    pub const fn tag0(&self) -> bool { self.tag0_or_false() }
    /// Sets the value of tag bit 0 to the given value.
    #[inline(always)]
    pub fn set_tag0(&mut self, tag0: bool) {
        let unmasked = self.0.get() & !1_usize;
        self.0 = unsafe { NonZeroUsize::new_unchecked(unmasked | tag0 as usize) };
    }
    /// Maps to the same handle but with tag bit 0 set to the given value.
    #[inline(always)]
    pub fn with_tag0(mut self, tag0: bool) -> Self {
        self.set_tag0(tag0);
        self
    }
}
impl<const TAG0: bool> AdvOwnedHandle<TAG0, true> {
    /// Returns the value of tag bit 1.
    #[inline(always)]
    pub const fn tag1(&self) -> bool { self.tag1_or_false() }
    /// Sets the value of tag bit 1 to the given value.
    #[inline(always)]
    pub fn set_tag1(&mut self, tag1: bool) {
        let unmasked = self.0.get() & !2_usize;
        self.0 = unsafe { NonZeroUsize::new_unchecked(unmasked | ((tag1 as usize) << 1)) };
    }
    /// Maps to the same handle but with tag bit 1 set to the given value.
    #[inline(always)]
    pub fn with_tag1(mut self, tag1: bool) -> Self {
        self.set_tag1(tag1);
        self
    }
}
impl AdvOwnedHandle<true, true> {
    /// Sets the tag bits to the given values.
    #[inline(always)]
    pub fn set_tags(&mut self, tag0: bool, tag1: bool) {
        self.set_tag0(tag0);
        self.set_tag1(tag1);
    }
    /// Maps to the same handle but with the tag bits set to the given values.
    #[inline(always)]
    pub fn with_tags(mut self, tag0: bool, tag1: bool) -> Self {
        self.set_tags(tag0, tag1);
        self
    }
}

// === handle access ===
impl<const TAG0: bool, const TAG1: bool> AsRawHandle for AdvOwnedHandle<TAG0, TAG1> {
    #[inline(always)]
    fn as_raw_handle(&self) -> RawHandle {
        // FUTURE use Strict Provenance API
        (self.0.get() & Self::TAG_UNMASK) as _
    }
}
impl<const TAG0: bool, const TAG1: bool> AsHandle for AdvOwnedHandle<TAG0, TAG1> {
    #[inline(always)]
    fn as_handle(&self) -> BorrowedHandle<'_> {
        unsafe { BorrowedHandle::borrow_raw(self.as_raw_handle()) }
    }
}
// === end handle access ===

// === constructors ===
impl From<OwnedHandle> for AdvOwnedHandle<false, false> {
    #[inline(always)]
    fn from(h: OwnedHandle) -> Self {
        Self(unsafe { NonZeroUsize::new_unchecked(h.into_raw_handle() as usize) })
    }
}
impl FromRawHandle for AdvOwnedHandle<false, false> {
    unsafe fn from_raw_handle(h: RawHandle) -> Self {
        Self::from(unsafe { OwnedHandle::from_raw_handle(h) })
    }
}

impl AdvOwnedHandle<true, false> {
    #[inline(always)]
    pub fn from_handle_tag0(h: OwnedHandle, tag0: bool) -> Self { Self::new(h, tag0, false) }
}
impl AdvOwnedHandle<false, true> {
    #[inline(always)]
    pub fn from_handle_tag0(h: OwnedHandle, tag1: bool) -> Self { Self::new(h, false, tag1) }
}
impl AdvOwnedHandle<true, true> {
    #[inline(always)]
    pub fn from_handle_tag0_tag1(h: OwnedHandle, tag0: bool, tag1: bool) -> Self {
        Self::new(h, tag0, tag1)
    }
}
// === end constructors ===

// === conversion to untagged handle ===
impl From<AdvOwnedHandle<true, false>> for AdvOwnedHandle<false, false> {
    #[inline(always)]
    fn from(ah: AdvOwnedHandle<true, false>) -> Self { Self::new(ah.into(), false, false) }
}
impl From<AdvOwnedHandle<false, true>> for AdvOwnedHandle<false, false> {
    #[inline(always)]
    fn from(ah: AdvOwnedHandle<false, true>) -> Self { Self::new(ah.into(), false, false) }
}
impl From<AdvOwnedHandle<true, true>> for AdvOwnedHandle<false, false> {
    #[inline(always)]
    fn from(ah: AdvOwnedHandle<true, true>) -> Self { Self::new(ah.into(), false, false) }
}
impl From<AdvOwnedHandle<true, true>> for AdvOwnedHandle<true, false> {
    #[inline(always)]
    fn from(ah: AdvOwnedHandle<true, true>) -> Self {
        let tag0 = ah.tag0();
        Self::new(ah.into(), tag0, false)
    }
}
impl From<AdvOwnedHandle<true, true>> for AdvOwnedHandle<false, true> {
    #[inline(always)]
    fn from(ah: AdvOwnedHandle<true, true>) -> Self {
        let tag1 = ah.tag1();
        Self::new(ah.into(), false, tag1)
    }
}
impl<const TAG0: bool, const TAG1: bool> From<AdvOwnedHandle<TAG0, TAG1>> for OwnedHandle {
    #[inline(always)]
    fn from(ah: AdvOwnedHandle<TAG0, TAG1>) -> Self {
        unsafe { Self::from_raw_handle(ManuallyDrop::new(ah).as_raw_handle()) }
    }
}
impl<const TAG0: bool, const TAG1: bool> IntoRawHandle for AdvOwnedHandle<TAG0, TAG1> {
    #[inline(always)]
    fn into_raw_handle(self) -> RawHandle { ManuallyDrop::new(self).as_raw_handle() }
}
// === end conversion to untagged handle ===

impl<const TAG0: bool, const TAG1: bool> TryClone for AdvOwnedHandle<TAG0, TAG1> {
    #[inline]
    fn try_clone(&self) -> io::Result<Self> {
        c_wrappers::duplicate_handle(self.as_handle())
            .map(|h| Self::new(h, self.tag0_or_false(), self.tag1_or_false()))
    }
}

impl<const TAG0: bool, const TAG1: bool> Debug for AdvOwnedHandle<TAG0, TAG1> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if !TAG0 && !TAG1 {
            return f.debug_tuple("OwnedHandle").field(&self.as_raw_handle()).finish();
        }
        let mut dt = f.debug_struct("OwnedHandle");
        dt.field("handle", &self.as_raw_handle());
        if TAG0 {
            dt.field("tag0", &self.tag0_or_false());
        }
        if TAG1 {
            dt.field("tag1", &self.tag1_or_false());
        }
        dt.finish()
    }
}
