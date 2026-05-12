use super::{super::QuotaExceeded, MsgBuf};
use core::{
    cmp::{max, min},
    mem::size_of,
    num::NonZeroUsize,
};

/// Capacity and reallocation.
impl MsgBuf<'_> {
    /// Returns the buffer's total capacity, including the already filled part.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    fn plan_grow_amortized(&self, target: NonZeroUsize) -> Result<usize, QuotaExceeded> {
        let quota = self.quota.unwrap_or(isize::MAX as usize);
        // The growth function. Grows exponentially and tries to never go under twice size of the
        // struct itself to prevent laughably small allocations.
        let grown_wrt_cap = max(target.get(), self.cap * 2);
        let grown = max(grown_wrt_cap, size_of::<MsgBuf>() * 2);
        let new_cap = min(grown, quota);
        if new_cap < target.get() {
            Err(QuotaExceeded { quota, attempted_alloc: target })
        } else {
            Ok(new_cap)
        }
    }

    /// Grows the buffer by an unspecified amount while retaining its content up to the fill cursor.
    /// A [`QuotaExceeded`] error is only possible if the current size of the buffer is exactly
    /// equal to the quota.
    ///
    /// For use in receive implementation loops that cannot anticipate the full length of the
    /// incoming message due to API limitations.
    ///
    /// The current implementation grows exponentially, with some additional minor heuristics.
    #[inline]
    pub fn grow(&mut self) -> Result<(), QuotaExceeded> {
        self.grow_to(self.cap + 1)
    }

    /// Same as [`.grow()`](Self::grow), but discards the filled part of the buffer.
    #[inline]
    pub fn clear_and_grow(&mut self) -> Result<(), QuotaExceeded> {
        self.clear_and_grow_to(self.cap + 1)
    }

    /// Ensures that the buffer has at least the given capacity, allocating if necessary and
    /// retaining its content up to the fill cursor.
    pub fn grow_to(&mut self, new_cap: usize) -> Result<(), QuotaExceeded> {
        let old_cap = self.cap;
        let fill = self.fill;
        let new_cap_exact =
            if let (true, Some(new_cap)) = (new_cap > old_cap, NonZeroUsize::new(new_cap)) {
                self.plan_grow_amortized(new_cap)?
            } else {
                return Ok(());
            };
        self.init = min(self.init, fill); // Avoids unnecessary copying
        let is_borrowed = self.borrow.is_some();
        let mut owned = self.take_owned().unwrap_or_default();
        let borrowed = is_borrowed.then(|| self.take_borrowed()).flatten();

        owned.grow(new_cap_exact); // This performs the safety check
        self.put_owned(owned);
        if let Some(borrowed) = borrowed {
            self[..fill].copy_from_slice(&borrowed[..fill]);
            unsafe {
                // SAFETY: it's the filled part of the old buffer
                self.set_init(fill);
            }
        }
        self.set_fill(fill);
        Ok(())
    }

    /// Wipes the contents of the buffer and ensures that it has at least the given capacity,
    /// allocating if necessary.
    #[inline]
    pub fn clear_and_grow_to(&mut self, new_cap: usize) -> Result<(), QuotaExceeded> {
        self.set_fill(0);
        self.grow_to(new_cap)
    }
}
