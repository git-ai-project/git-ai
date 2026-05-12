use super::{owned::OwnedBuf, owned_default, MsgBuf};

/// Lifetime management.
impl MsgBuf<'_> {
    /// Makes sure `self` is owned by making a new allocation equal in size to the borrowed
    /// capacity if it is borrowed. Discards data in `self` if a reallocation is entailed.
    pub fn make_owned<Owned: OwnedBuf>(self) -> MsgBuf<'static> {
        self.try_extend_lifetime().unwrap_or_else(|slf| {
            let mut owned = owned_default::<Owned>();
            owned.grow(slf.cap);
            let mut buf = MsgBuf::from(owned);
            buf.quota = slf.quota;
            buf
        })
    }
    /// Attempts to extend lifetime to `'static`, failing if the buffer is borrowed.
    pub fn try_extend_lifetime(self) -> Result<MsgBuf<'static>, Self> {
        if self.borrow.is_none() || self.cap == 0 {
            let Self { ptr, cap, quota, init, borrow: _, own_vt, fill, has_msg } = self;
            Ok(MsgBuf { ptr, cap, quota, init, borrow: None, own_vt, fill, has_msg })
        } else {
            Err(self)
        }
    }
}
