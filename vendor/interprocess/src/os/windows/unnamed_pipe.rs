//! Windows-specific functionality for unnamed pipes.

#[cfg(feature = "tokio")]
#[cfg_attr(feature = "doc_cfg", doc(cfg(feature = "tokio")))]
pub mod tokio;

use {
    crate::{
        os::windows::{c_wrappers, security_descriptor::*, winprelude::*},
        ref2ptr,
        unnamed_pipe::{Recver as PubRecver, Sender as PubSender},
        Sealed, TryClone,
    },
    std::{
        fmt::{self, Debug, Formatter},
        io::{self, Read, Write},
        mem::ManuallyDrop,
        num::NonZeroUsize,
    },
    windows_sys::Win32::System::Pipes::CreatePipe,
};

/// Builder used to create unnamed pipes while supplying additional options.
///
/// You can use this instead of the simple [`pipe` function](crate::unnamed_pipe::pipe) to supply
/// additional Windows-specific parameters to a pipe.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct CreationOptions<'sd> {
    /// Security descriptor for the pipe.
    pub security_descriptor: Option<BorrowedSecurityDescriptor<'sd>>,
    /// Specifies whether the resulting pipe can be inherited by child processes.
    ///
    /// The default value is `true`.
    pub inheritable: bool,
    /// Hint on the buffer size for the pipe. There is no way to ensure or check that the system
    /// actually uses this exact size, since it's only a hint. Set to `None` to disable the hint
    /// and rely entirely on the system's default buffer size.
    pub buffer_size_hint: Option<NonZeroUsize>,
}
impl Sealed for CreationOptions<'_> {}
impl<'sd> CreationOptions<'sd> {
    /// Starts with the default parameters for the pipe. Identical to `Default::default()`.
    pub const fn new() -> Self {
        Self { inheritable: true, security_descriptor: None, buffer_size_hint: None }
    }

    builder_setters! {
        /// Specifies the pointer to the security descriptor for the pipe.
        ///
        /// See the [associated field](#structfield.security_descriptor) for more.
        security_descriptor: Option<BorrowedSecurityDescriptor<'sd>>,
        /// Specifies whether the resulting pipe can be inherited by child processes.
        ///
        /// See the [associated field](#structfield.inheritable) for more.
        inheritable: bool,
        /// Provides Windows with a hint for the buffer size for the pipe.
        ///
        /// See the [associated field](#structfield.buffer_size_hint) for more.
        buffer_size_hint: Option<NonZeroUsize>,
    }

    /// Creates the pipe and returns its sending and receiving ends, or an error if one occurred.
    pub fn create(self) -> io::Result<(PubSender, PubRecver)> {
        let hint_raw = match self.buffer_size_hint {
            Some(num) => num.get(),
            None => 0,
        }
        .try_into()
        .unwrap();

        let sd = create_security_attributes(self.security_descriptor, self.inheritable);

        let [mut w, mut r] = [INVALID_HANDLE_VALUE; 2];
        let success =
            unsafe { CreatePipe(&mut r, &mut w, ref2ptr(&sd).cast_mut().cast(), hint_raw) } != 0;
        if success {
            let (w, r) = unsafe {
                // SAFETY: we just created those handles which means that we own them
                let w = OwnedHandle::from_raw_handle(w);
                let r = OwnedHandle::from_raw_handle(r);
                (w, r)
            };
            let w = PubSender(Sender { io: ManuallyDrop::new(w.into()), needs_flush: false });
            let r = PubRecver(Recver(r.into()));
            Ok((w, r))
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Synonymous with [`.create()`](Self::create).
    #[inline]
    pub fn build(self) -> io::Result<(PubSender, PubRecver)> { self.create() }
}
impl Default for CreationOptions<'_> {
    fn default() -> Self { Self::new() }
}

pub(crate) fn pipe_impl() -> io::Result<(PubSender, PubRecver)> {
    CreationOptions::default().build()
}

pub(crate) struct Recver(AdvOwnedHandle);
forward_handle!(Recver);
impl Read for Recver {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        c_wrappers::read(self.as_handle(), buf)
    }
}
impl Debug for Recver {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Recver").field(&self.0.as_raw_handle()).finish()
    }
}
impl TryClone for Recver {
    #[inline]
    fn try_clone(&self) -> io::Result<Self> {
        c_wrappers::duplicate_handle(self.as_handle()).map(AdvOwnedHandle::from).map(Self)
    }
}

#[derive(Debug)]
pub(crate) struct Sender {
    io: ManuallyDrop<AdvOwnedHandle>,
    needs_flush: bool,
}
impl Write for Sender {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let rslt = c_wrappers::write(self.as_handle(), buf);
        if rslt.is_ok() {
            self.needs_flush = true;
        }
        rslt
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        if self.needs_flush {
            let rslt = c_wrappers::flush(self.as_handle());
            if rslt.is_ok() {
                self.needs_flush = false;
            }
            rslt
        } else {
            Ok(())
        }
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        let h = unsafe { ManuallyDrop::take(&mut self.io) };
        if self.needs_flush {
            linger_pool::linger(h);
        }
    }
}
impl TryClone for Sender {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { io: ManuallyDrop::new(self.io.try_clone()?), needs_flush: self.needs_flush })
    }
}
impl AsHandle for Sender {
    #[inline]
    fn as_handle(&self) -> BorrowedHandle<'_> { self.io.as_handle() }
}
impl From<OwnedHandle> for Sender {
    #[inline]
    fn from(handle: OwnedHandle) -> Self {
        Self { io: ManuallyDrop::new(handle.into()), needs_flush: true }
    }
}
impl From<Sender> for OwnedHandle {
    #[inline]
    fn from(tx: Sender) -> Self {
        unsafe { ManuallyDrop::take(&mut ManuallyDrop::new(tx).io).into() }
    }
}
