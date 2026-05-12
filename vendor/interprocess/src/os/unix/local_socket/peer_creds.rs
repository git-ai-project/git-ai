use {
    crate::os::unix::{c_wrappers, unixprelude::*},
    std::{io, mem::MaybeUninit},
};

pub type Pid = pid_t;

#[derive(Copy, Clone, Debug)]
pub struct PeerCreds(Inner);
impl PeerCreds {
    #[cfg(any(
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
    ))]
    #[allow(clippy::cast_sign_loss)]
    const CR_VERSION_OFFSET: usize = unsafe {
        let cred = std::mem::zeroed::<libc::xucred>();
        crate::ref2ptr(&cred.cr_version)
            .cast::<()>()
            .byte_offset_from(crate::ref2ptr(&cred).cast::<()>()) as usize
    };
    pub(crate) fn for_socket(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let inner = unsafe {
            c_wrappers::getsockopt::<MaybeUninit<Inner>>(fd, CRED_OPTLEVEL, CRED_OPTNAME)?
        };
        #[cfg(any(
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
        ))]
        {
            let vers = unsafe {
                (&inner as *const MaybeUninit<Inner>)
                    .byte_add(Self::CR_VERSION_OFFSET)
                    .cast::<libc::c_uint>()
                    .read()
            };
            #[allow(clippy::absurd_extreme_comparisons)]
            if vers < libc::XUCRED_VERSION {
                // The manpage tells me to check the value but doesn't say what I am to do in
                // the case of a mismatch. If the FreeBSD folks can get away with doing the bare
                // minimum, I might as well do the same thing.
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
        }
        let inner = unsafe { inner.assume_init() };
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "redox",
            target_os = "fuchsia"
        ))]
        if inner.pid == 0 {
            // Yes, a Linux kernel developer really thought that zero-initializing a struct that
            // contains a UID field was a good mechanism for representing an obscure sentinel
            return Err(io::Error::from(io::ErrorKind::ConnectionReset));
        }
        Ok(Self(inner))
    }
    pub fn pid(&self) -> Option<pid_t> {
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "redox",
            target_os = "fuchsia",
            target_os = "openbsd",
        ))]
        return Some(self.0.pid);
        #[cfg(target_os = "freebsd")]
        // SAFETY: we've checked the version of the structure
        return Some(unsafe { self.0.cr_pid__c_anonymous_union.cr_pid });
        #[cfg(target_os = "netbsd")]
        return Some(self.0.unp_pid);
        #[allow(unreachable_code)]
        None
    }
    pub fn euid(&self) -> Option<uid_t> {
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "redox",
            target_os = "fuchsia",
            target_os = "openbsd",
        ))]
        return Some(self.0.uid);
        #[cfg(any(
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
        ))]
        return Some(self.0.cr_uid);
        #[cfg(target_os = "netbsd")]
        return Some(self.0.unp_euid);
        #[allow(unreachable_code)]
        None
    }
    pub fn egid(&self) -> Option<uid_t> {
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "redox",
            target_os = "fuchsia",
            target_os = "openbsd",
        ))]
        return Some(self.0.gid);
        #[cfg(target_os = "netbsd")]
        return Some(self.0.unp_egid);
        #[allow(unreachable_code)]
        None
    }
    pub fn groups(&self) -> Option<&[gid_t]> {
        #[cfg(any(
            target_os = "freebsd",
            target_os = "dragonfly",
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
        ))]
        #[allow(clippy::cast_sign_loss, clippy::indexing_slicing)]
        return Some(&self.0.cr_groups[..self.0.cr_ngroups as usize]);
        #[allow(unreachable_code)]
        None
    }
}

#[cfg(target_os = "openbsd")]
use libc::{sockpeercred as Inner, SOL_SOCKET as CRED_OPTLEVEL, SO_PEERCRED as CRED_OPTNAME};
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "redox",
    target_os = "fuchsia",
))]
use libc::{ucred as Inner, SOL_SOCKET as CRED_OPTLEVEL, SO_PEERCRED as CRED_OPTNAME};
#[cfg(target_os = "netbsd")]
use {
    libc::{unpcbid as Inner, LOCAL_PEEREID as CRED_OPTNAME},
    SOL_LOCAL_LIBC_CRATE_DOESNT_HAVE_IT as CRED_OPTLEVEL,
};
#[cfg(any(
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
))]
use {
    libc::{xucred as Inner, LOCAL_PEERCRED as CRED_OPTNAME},
    SOL_LOCAL_LIBC_CRATE_DOESNT_HAVE_IT as CRED_OPTLEVEL,
};

#[allow(unused)]
const SOL_LOCAL_LIBC_CRATE_DOESNT_HAVE_IT: c_int = 0;
