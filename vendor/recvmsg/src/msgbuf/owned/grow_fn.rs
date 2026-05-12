use super::{OwnedBuf, OwnedBufRawParts};
use core::{
    fmt::{self, Debug, Formatter},
    marker::PhantomData,
};

/// Custom allocation of [owned buffers](OwnedBuf).
///
/// It makes no sense for implementors to not be zero-sized (though they can be never-sized, at your
/// discretion).
pub trait GrowFn: 'static {
    /// Grows the buffer up to the given capacity.
    ///
    /// Does not necessarily have to be able to decrease the buffer's capacity.
    fn grow<Owned: OwnedBuf>(owned: &mut Owned, new_cap: usize);
}

/// The default [growth function](GrowFn) of an [owned buffer](OwnedBuf).
pub struct DefaultFn(());
impl GrowFn for DefaultFn {
    #[inline]
    fn grow<Owned: OwnedBuf>(owned: &mut Owned, new_cap: usize) {
        owned.grow(new_cap);
    }
}

/// Applies the given growth function to the given owned buffer type.
pub struct WithGrowFn<Owned, Gfn>(pub Owned, PhantomData<fn(Gfn)>);
impl<Owned, Gfn> From<Owned> for WithGrowFn<Owned, Gfn> {
    #[inline]
    fn from(owned: Owned) -> Self {
        Self(owned, PhantomData)
    }
}
unsafe impl<Owned: OwnedBuf, Gfn: GrowFn> OwnedBuf for WithGrowFn<Owned, Gfn> {
    #[inline]
    unsafe fn from_raw_parts(raw_parts: OwnedBufRawParts) -> Self {
        let owned = unsafe { Owned::from_raw_parts(raw_parts) };
        Self(owned, PhantomData)
    }
    #[inline]
    fn as_raw_parts(&self) -> super::OwnedBufRawParts {
        self.0.as_raw_parts()
    }
    #[inline]
    fn grow(&mut self, new_cap: usize) {
        Gfn::grow(&mut self.0, new_cap);
    }
}
impl<Owned: Default, Gfn> Default for WithGrowFn<Owned, Gfn> {
    #[inline]
    fn default() -> Self {
        Self(Owned::default(), PhantomData)
    }
}
impl<Owned: Debug, Gfn> Debug for WithGrowFn<Owned, Gfn> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("WithGrowFn").field(&self.0).finish()
    }
}
