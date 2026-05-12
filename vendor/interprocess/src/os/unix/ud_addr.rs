use {
    crate::{os::unix::unixprelude::*, weaken_nonzero_slice},
    libc::sockaddr_un,
    std::{
        ffi::CStr,
        io,
        mem::{size_of, zeroed, MaybeUninit},
        num::NonZeroU8,
        ops::Deref,
        ptr::{addr_of, addr_of_mut, copy_nonoverlapping},
        slice,
    },
};

#[cold]
#[inline(never)]
pub(super) fn name_too_long() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "local socket name length exceeds capacity of sun_path of sockaddr_un",
    )
}

pub(super) const SUN_LEN: usize = {
    let sun = unsafe { zeroed::<sockaddr_un>() };
    sun.sun_path.len()
};
const PATH_OFFSET: usize = {
    let sun = unsafe { zeroed::<sockaddr_un>() };
    let sunptr = (&sun as *const sockaddr_un).cast::<u8>();

    // FUTURE use offset_from_unsigned
    #[allow(clippy::cast_sign_loss)]
    let off = unsafe { addr_of!(sun.sun_path).cast::<u8>().offset_from(sunptr) } as usize;
    if off + sun.sun_path.len() != size_of::<sockaddr_un>() {
        panic!("unsupported sockaddr_un layout");
    }
    off
};

/// Wrapper around `sockaddr_un` that remediates the null termination edge case.
#[derive(Copy, Clone)]
#[repr(C)]
pub(super) struct UdAddr {
    len: socklen_t,
    sun: MaybeUninit<sockaddr_un>,
    // We know thanks to the check down below in PATH_OFFSET that
    // this immediately follows the last byte of sun_path.
    terminator: MaybeUninit<c_char>,
}
/// Creation and accessors.
impl UdAddr {
    /// Creates an empty `UdAddr`.
    #[inline]
    pub fn new() -> Self {
        let mut sun = MaybeUninit::<sockaddr_un>::uninit();
        #[allow(clippy::cast_possible_truncation)]
        unsafe {
            addr_of_mut!((*sun.as_mut_ptr()).sun_family).write(libc::AF_UNIX as _)
        };
        Self { len: 0, sun, terminator: MaybeUninit::uninit() }
    }

    /// Returns a constant pointer to the `sun_path` field.
    ///
    /// If one of the initialization methods has been called, this points to a nul-terminated C
    /// string.
    #[inline]
    pub fn path_ptr(&self) -> *const u8 {
        // SAFETY: known to be in bounds as per derivation of PATH_OFFSET
        unsafe { self.sun.as_ptr().cast::<u8>().add(PATH_OFFSET) }
    }
    /// Returns a mutable pointer to the `sun_path` field.
    ///
    /// If one of the initialization methods has been called, this points to a nul-terminated C
    /// string.
    #[inline]
    pub fn path_ptr_mut(&mut self) -> *mut u8 { self.path_ptr().cast_mut() }

    /// Mutably borrows the `sun_path` field.
    ///
    /// # Safety
    /// You must not use this to de-initialize bytes within the initialization cursor.
    #[inline]
    pub unsafe fn path_buf_mut(&mut self) -> &mut [MaybeUninit<u8>; SUN_LEN] {
        unsafe { &mut *self.path_ptr_mut().cast() }
    }

    /// Immutably borrows the initialized part of `sun_path`.
    #[inline]
    pub fn path(&self) -> &[u8] { unsafe { slice::from_raw_parts(self.path_ptr(), self.len()) } }
    /// Mutably borrows the initialized part of `sun_path`.
    #[inline]
    pub fn path_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.path_ptr_mut(), self.len()) }
    }

    /// Returns a pointer to the `sockaddr_un` structure that can be passed to `bind`.
    ///
    /// Before passing this pointer to `bind`, [`write_terminator`](Self::write_terminator)
    /// needs to be called first.
    pub fn addr_ptr(&self) -> *const sockaddr_un { self.sun.as_ptr() }

    /// Returns the address length that is to be passed to `bind`. This is different from the
    /// [initialized length](Self::len).
    #[allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]
    #[inline]
    pub fn addrlen(&self) -> socklen_t { self.len + PATH_OFFSET as socklen_t }

    /// Returns the initialized length, which is the number of bytes at the beginning of the
    /// path buffer that are initialized.
    #[inline]
    pub fn len(&self) -> usize { self.len as usize }
    /// Resets the [initialized length](Self::len) to zero.
    #[inline]
    pub fn reset_len(&mut self) { self.len = 0 }
    /// Sets the [initialized length](Self::len) to the given value.
    ///
    /// # Safety
    /// At least this many bytes must be initialized at the beginning of the buffer, and
    /// it must not exceed the bounds of the buffer.
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    pub unsafe fn set_len(&mut self, len: usize) { self.len = len as socklen_t }
    /// Increments the [initialized length](Self::len) by the given value.
    ///
    /// # Safety
    /// See [`set_len`](Self::set_len).
    #[inline]
    #[allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]
    pub unsafe fn incr_len(&mut self, incr: usize) { self.len += incr as socklen_t }
}

/// Initialization.
impl UdAddr {
    /// Appends the given slice to the buffer.
    ///
    /// # Safety
    /// The sum of the initialized length and the length of the slice must not exceed the size of
    /// the `sun_path` field.
    #[inline]
    pub unsafe fn push_slice(&mut self, s: &[NonZeroU8]) {
        unsafe { self.push_slice_with_nuls(weaken_nonzero_slice(s)) };
    }

    /// Like `push_slice`, but allows interior nuls. Behavior is unspecified if they are not later
    /// dealt with.
    ///
    /// # Safety
    /// Same as `push_slice`.
    pub unsafe fn push_slice_with_nuls(&mut self, s: &[u8]) {
        unsafe { self.write_slice(self.len as usize, s) };
        unsafe { self.incr_len(s.len()) };
    }

    /// Writes a nul terminator, making `path_ptr` point to a nul-terminated string. A witness
    /// object is returned that codifies in the type system that nul termination has been
    /// established.
    pub fn write_terminator(&mut self) -> TerminatedUdAddr<'_> {
        let len = self.len as usize;
        if len < SUN_LEN {
            unsafe { self.path_ptr_mut().add(len).write(0) };
        } else {
            self.terminator = MaybeUninit::new(0);
        }
        TerminatedUdAddr(self)
    }

    fn check_path_length(len: usize) -> io::Result<()> {
        let true = len <= SUN_LEN else { return Err(name_too_long()) };
        Ok(())
    }
    unsafe fn write_slice(&mut self, off: usize, s: &[u8]) {
        unsafe { copy_nonoverlapping(s.as_ptr(), self.path_ptr_mut().add(off), s.len()) };
    }

    /// Initializes from a regular path.
    pub fn init(&mut self, path: &[NonZeroU8]) -> io::Result<()> {
        Self::check_path_length(path.len())?;
        unsafe { self.push_slice(path) };
        Ok(())
    }
    /// Initializes from an abstract namespace name.
    #[allow(dead_code, clippy::arithmetic_side_effects)]
    pub fn init_namespaced(&mut self, nsname: &[NonZeroU8]) -> io::Result<()> {
        // Cannot overflow, as the length of slices always fits into an isize
        Self::check_path_length(nsname.len() + 1)?;
        unsafe { self.path_ptr_mut().write(0) };
        self.len = 1;
        unsafe { self.push_slice(nsname) };
        Ok(())
    }
}

/// Reference to a [`UdAddr`] that is known to be nul-terminated.
#[derive(Copy, Clone)]
pub(super) struct TerminatedUdAddr<'a>(&'a UdAddr);
impl<'a> TerminatedUdAddr<'a> {
    /// Grants read-only access to the [`UdAddr`].
    pub const fn inner(self) -> &'a UdAddr { self.0 }
    /// Immutably borrows the path as a nul-terminated C string.
    pub fn path(&self) -> &CStr {
        // SAFETY: the nul terminator is either in sun_path or immediately follows it
        unsafe { CStr::from_ptr(self.0.path_ptr().cast()) }
    }
}
impl Deref for TerminatedUdAddr<'_> {
    type Target = UdAddr;
    fn deref(&self) -> &UdAddr { self.0 }
}
