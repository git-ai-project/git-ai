//! Forwarding of `AsRef` and `AsMut` for newtypes. Is also a derive macro in some sense.

macro_rules! forward_as_ref {
    ($({$($lt:tt)*})? $ty:ty, $tgt:ty) => {
        impl $(<$($lt)*>)? ::core::convert::AsRef<$tgt> for $ty {
            #[inline(always)]
            fn as_ref(&self) -> &$tgt { &self.0 }
        }
    };
}
macro_rules! forward_as_mut {
    ($({$($lt:tt)*})? $ty:ty, $tgt:ty) => {
        impl $(<$($lt)*>)? ::core::convert::AsMut<$tgt> for $ty {
            #[inline(always)]
            fn as_mut(&mut self) -> &mut $tgt { &mut self.0 }
        }
    };
}
