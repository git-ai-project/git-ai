use {
    crate::ref2ptr,
    std::{mem::ManuallyDrop, ptr, sync::Arc},
};

pub trait OptArc:
    From<Self::Value> + From<Arc<Self::Value>> + Into<MaybeArc<Self::Value>> + Send + Sync + Sized
{
    type Value: Send + Sync + Sized;
    fn get(&self) -> &Self::Value;
    fn get_arc(&self) -> Option<&Arc<Self::Value>>;
    fn refclone(&mut self) -> Self;
    fn try_make_owned(&mut self) -> bool;
    fn ptr_eq(&self, other: &impl OptArc<Value = Self::Value>) -> bool {
        match (self.get_arc(), other.get_arc()) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => std::ptr::eq(ref2ptr(self).cast::<()>(), ref2ptr(other).cast()),
            _ => false,
        }
    }
}
pub trait OptArcIRC: OptArc {
    fn refclone(&self) -> Self;
}

impl<T: Send + Sync> OptArc for Arc<T> {
    type Value = T;
    #[inline(always)]
    fn get(&self) -> &T { self }
    #[inline(always)]
    fn get_arc(&self) -> Option<&Arc<T>> { Some(self) }
    #[inline(always)]
    fn refclone(&mut self) -> Self { Arc::clone(self) }
    #[inline(always)]
    fn try_make_owned(&mut self) -> bool { false }
}
impl<T: Send + Sync> OptArcIRC for Arc<T> {
    #[inline(always)]
    fn refclone(&self) -> Self { Arc::clone(self) }
}

/// Inlining optimization for `Arc`.
#[derive(Debug)]
pub enum MaybeArc<T> {
    Inline(T),
    Shared(Arc<T>),
}
impl<T> From<T> for MaybeArc<T> {
    #[inline]
    fn from(x: T) -> Self { Self::Inline(x) }
}
impl<T> From<Arc<T>> for MaybeArc<T> {
    #[inline]
    fn from(a: Arc<T>) -> Self { Self::Shared(a) }
}
impl<T: Send + Sync> OptArc for MaybeArc<T> {
    type Value = T;
    #[inline]
    fn get(&self) -> &Self::Value {
        match self {
            Self::Inline(v) => v,
            Self::Shared(a) => a,
        }
    }
    #[inline]
    fn get_arc(&self) -> Option<&Arc<T>> {
        match self {
            Self::Inline(..) => None,
            Self::Shared(a) => Some(a),
        }
    }
    fn refclone(&mut self) -> Self {
        Self::Shared(match self {
            Self::Inline(mx) => {
                // SAFETY: reading a reference, ManuallyDrop precludes double free
                let arc = Arc::new(unsafe { ManuallyDrop::new(ptr::read(mx)) });
                // Begin no-diverge zone
                // SAFETY: ManuallyDrop is layout-transparent
                let arc = unsafe { Arc::from_raw(Arc::into_raw(arc).cast::<T>()) };
                // SAFETY: self is a reference, so is valid for writes and aligned
                unsafe { ptr::write(self, Self::Shared(arc)) };
                // End no-diverge zone

                let ref_for_clone = match self {
                    Self::Shared(s) => &*s,
                    Self::Inline(..) => unsafe { std::hint::unreachable_unchecked() },
                };
                Arc::clone(ref_for_clone)
            }
            Self::Shared(arc) => Arc::clone(arc),
        })
    }
    fn try_make_owned(&mut self) -> bool {
        let Self::Shared(a) = self else { return true };
        // Begin no-diverge zone
        // SAFETY: we consume or forget the Arc before returning
        match Arc::try_unwrap(unsafe { ptr::read(a) }) {
            Ok(o) => {
                unsafe { ptr::write(self, Self::Inline(o)) };
                // End no-diverge zone (copied Arc consumed)
                true
            }
            Err(a) => {
                let _ = ManuallyDrop::new(a);
                // End no-diverge zone (copied Arc forgotten)
                false
            }
        }
    }
}
