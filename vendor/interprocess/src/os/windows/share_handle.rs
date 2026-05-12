use {
    super::{c_wrappers, winprelude::*},
    std::io,
};

/// Objects which own handles which can be shared with other processes.
///
/// On Windows, like on most other operating systems, handles belong to specific processes. You
/// shouldn't just send the value of a handle to another process (with a named pipe, for example)
/// and expect it to work on the other side. For this to work, you need
/// [`DuplicateHandle`](windows_sys::Win32::Foundation::DuplicateHandle) – the Win32 API function
/// which duplicates a handle into the handle table of the specified process (the receiver is
/// referred to by its handle). This trait exposes the `DuplicateHandle` functionality in a safe
/// manner.
///
/// Note that the resulting handle is expected not to be inheritable. It is a logic error to have
/// the output of `.share()` be inheritable, but it is not UB.
///
/// **Implemented for all types inside this crate which implement [`AsHandle`] and are supposed to
/// be shared between processes.**
pub trait ShareHandle: AsHandle {
    /// Duplicates the handle to make it accessible in the specified process (taken as a handle to
    /// that process) and returns the raw value of the handle which can then be sent via some form
    /// of IPC, typically named pipes. This is the only way to use any form of IPC other than named
    /// pipes to communicate between two processes which do not have a parent-child relationship or
    /// if the handle wasn't created as inheritable.
    ///
    /// Backed by [`DuplicateHandle`](windows_sys::Win32::Foundation::DuplicateHandle). Doesn't
    /// require unsafe code since `DuplicateHandle` never leads to undefined behavior if the
    /// `lpTargetHandle` parameter is a valid pointer, only creates an error.
    fn share(&self, receiver: BorrowedHandle<'_>) -> io::Result<RawHandle> {
        c_wrappers::duplicate_handle_to_foreign(self.as_handle(), receiver)
    }
}
impl ShareHandle for crate::unnamed_pipe::Recver {}
impl ShareHandle for crate::unnamed_pipe::Sender {}
