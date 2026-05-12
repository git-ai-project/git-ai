use {
    crate::{DebugExpectExt, OrErrno, ToBool},
    std::fmt::{self, Debug},
    windows_sys::Win32::Security::RevertToSelf,
};

/// [Reverts impersonation][rd] when dropped.
///
/// [rd]: https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-reverttoself
pub struct ImpersonationGuard(pub(crate) ());
impl Drop for ImpersonationGuard {
    fn drop(&mut self) {
        unsafe { RevertToSelf() }
            .to_bool()
            .true_val_or_errno(())
            .debug_expect("failed to revert impersonation")
    }
}
impl Debug for ImpersonationGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("ImpersonationGuard") }
}
