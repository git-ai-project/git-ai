//! Local sockets implemented using Unix domain sockets.

mod listener;
mod stream;

pub use {listener::*, stream::*};

/// Async Local sockets for Tokio implemented using Unix domain sockets.
#[cfg(feature = "tokio")]
pub mod tokio {
    mod listener;
    mod stream;
    pub use {listener::*, stream::*};
}

use {
    crate::{
        assume_nonzero_slice, check_nonzero_slice,
        local_socket::{ListenerOptions, Name, NameInner},
        os::unix::{
            c_wrappers,
            ud_addr::{name_too_long, TerminatedUdAddr, UdAddr, SUN_LEN},
            unixprelude::*,
        },
        timeout_expiry,
    },
    std::{
        ffi::{CStr, OsStr},
        fmt::{self, Debug, Formatter},
        io,
        mem::MaybeUninit,
        num::NonZeroU8,
        path::Path,
        time::{Duration, Instant},
    },
};

const CONN_TIMEOUT_MSG: &str = "timed out while connecting to local socket server";

/// Performs name reclamation when dropped.
#[derive(Clone, Default)]
struct ReclaimGuard(Box<[u8]>);
impl ReclaimGuard {
    fn disarmed() -> Self { Self(Box::new([])) }
    /// Creates a reclamation guard for the given address. If `cond` is false, creates a disarmed
    /// guard instead.
    fn new(cond: bool, addr: TerminatedUdAddr<'_>) -> Self {
        if !cond
            || addr.inner().path().is_empty()
            || cfg!(any(target_os = "linux", target_os = "android"))
                && addr.inner().path().first() == Some(&0)
        {
            return Self::disarmed();
        }
        Self(addr.path().to_owned().into_bytes_with_nul().into_boxed_slice())
    }
    /// Takes ownership of the reclaim guard, leaving a disarmed one in place.
    #[cfg_attr(not(feature = "tokio"), allow(dead_code))]
    fn take(&mut self) -> Self { Self(std::mem::take(&mut self.0)) }
    /// Disarms the reclaim guard. It will not do anything when dropped.
    fn forget(&mut self) { self.0 = Box::new([]); }
    fn as_c_str(&self) -> Option<&CStr> {
        // SAFETY: the only constructor that produces a non-empty one gets
        //         it from into_bytes_with_nul
        (!self.0.is_empty()).then(|| unsafe { CStr::from_bytes_with_nul_unchecked(&self.0) })
    }
}
impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        if let Some(s) = self.as_c_str() {
            let _ = c_wrappers::unlink(s);
        }
    }
}
impl Debug for ReclaimGuard {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let s = self.as_c_str().map(|s| OsStr::from_bytes(s.to_bytes()));
        f.debug_tuple("ReclaimGuard").field(&s).finish()
    }
}

/// Calls the given listener closure using `dispatch_name` to try every applicable path.
/// If `try_overwrite` is enabled, this is repeated in a loop for every path offered by
/// `dispatch_name` for as long as listener creation fails with `AddrInUse` with attempts to
/// unlink the offending socket file interspersed between attempts to bind.
///
/// If unlinking fails for a reason other than the socket file already having been deleted, the
/// loop is exited and the unlink error is propagated.
///
/// The spin time limit is respected when present.
fn listen_and_maybe_overwrite<T>(
    mut opts: ListenerOptions<'_>,
    mut listen: impl FnMut(TerminatedUdAddr<'_>, &mut ListenerOptions<'_>) -> io::Result<T>,
) -> io::Result<T> {
    let end = opts.get_max_spin_time().map(timeout_expiry).transpose()?;
    dispatch_name(
        &mut opts,
        true,
        |opts| opts.name.borrow(),
        |opts| opts.get_max_spin_time_mut(),
        |addr, opts| {
            let mut first = true;
            loop {
                let err = match listen(addr, opts) {
                    Err(e) if keep_trying_to_overwrite(&e, opts) => e,
                    otherwise => break otherwise,
                };
                if !continue_spin_loop(end, opts.get_max_spin_time_mut()) && !first {
                    break Err(err);
                }
                first = false;
                unlink_and_eat_noents(addr)?;
            }
        },
    )
}

fn unlink_and_eat_noents(addr: TerminatedUdAddr<'_>) -> io::Result<()> {
    match c_wrappers::unlink(addr.path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn keep_trying_to_overwrite(e: &io::Error, options: &ListenerOptions<'_>) -> bool {
    options.get_try_overwrite() && e.kind() == io::ErrorKind::AddrInUse
}

fn check_no_nul(s: &[u8]) -> io::Result<&[NonZeroU8]> {
    let msg = "interior nul bytes are not allowed inside Unix domain socket names";
    check_nonzero_slice(s).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, msg))
}

/// Calls the given closure once for every applicable Unix domain socket address corresponding to
/// the given local socket name. If `create_dirs` is true and the closure returns a
/// ["benign"](fail_is_benign) error, missing directories are created and the call is retried.
fn dispatch_name<O, T>(
    o: &mut O,
    create_dirs: bool,
    mut get_name: impl FnMut(&mut O) -> Name<'_>,
    mut max_spin_time: impl FnMut(&mut O) -> Option<&mut Duration>,
    mut create: impl FnMut(TerminatedUdAddr<'_>, &mut O) -> io::Result<T>,
) -> io::Result<T> {
    let mut addr = UdAddr::new();
    match get_name(o).0 {
        NameInner::UdSocketPath(path) => {
            addr.init(check_no_nul(path.as_bytes())?)?;
            create(addr.write_terminator(), o)
        }

        NameInner::UdSocketPseudoNs(name) => {
            let name = name.as_bytes();
            write_run_user(&mut addr, name)?;
            match with_missing_dir_creat(
                o,
                create_dirs,
                addr.write_terminator(),
                &mut max_spin_time,
                &mut create,
            ) {
                Err(e) if fail_is_benign(&e) => {
                    // borrow checker appeasement
                    let NameInner::UdSocketPseudoNs(name) = get_name(o).0 else { unreachable!() };
                    write_prefixed(&mut addr, tmpdir(), name.as_bytes())?;
                    with_missing_dir_creat(
                        o,
                        create_dirs,
                        addr.write_terminator(),
                        &mut max_spin_time,
                        &mut create,
                    )
                }
                otherwise => otherwise,
            }
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        NameInner::UdSocketNs(name) => {
            addr.init_namespaced(check_no_nul(&name)?)?;
            create(addr.write_terminator(), o)
        }
    }
}

/// Calls the given listener creation closure while handling failure by attempting to
/// [create missing directories](create_missing_dirs) if `create` is true.
fn with_missing_dir_creat<O, T>(
    options: &mut O,
    create: bool,
    addr: TerminatedUdAddr<'_>,
    mut max_spin_time: impl FnMut(&mut O) -> Option<&mut Duration>,
    mut f: impl FnMut(TerminatedUdAddr<'_>, &mut O) -> io::Result<T>,
) -> io::Result<T> {
    let end = max_spin_time(options).copied().map(timeout_expiry).transpose()?;
    let mut first = true;
    loop {
        let err = match f(addr, options) {
            Err(e) if create && fail_is_benign(&e) => e,
            otherwise => return otherwise,
        };
        if !continue_spin_loop(end, max_spin_time(options)) && !first {
            break Err(err);
        }
        first = false;
        create_missing_dirs(addr).then_some(()).ok_or(err)?;
    }
}

/// Makes it so that attempting to bind to the given address does not `ENOENT` assuming lack of
/// an asshole that races us and `rmdir`s what we've just created. Returns `false` in case of
/// failure.
fn create_missing_dirs(addr: TerminatedUdAddr<'_>) -> bool {
    let path = Path::new(OsStr::from_bytes(addr.inner().path()));
    // This is the reason we erase the error
    let Some(dir) = path.parent() else { return false };
    let false = dir.as_os_str().is_empty() else { return false };
    match std::fs::create_dir_all(dir) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => true,
        Err(..) => false,
    }
}

/// Updates `spin_time` with the amount of time remaining until `end` is reached. If `end` has
/// been reached, returns `false` and sets `spin_time` to zero; otherwise, returns `true`.
fn continue_spin_loop(end: Option<Instant>, spin_time: Option<&mut Duration>) -> bool {
    let Some(end) = end else { return false };
    let cur = Instant::now();
    if cur >= end {
        spin_time.map(|time| *time = Duration::ZERO);
        return false;
    }
    spin_time.map(|time| *time = end.saturating_duration_since(cur));
    true
}

const MAX_RUN_USER: usize = "/run/user//".len() + uid_t::MAX.ilog10() as usize + 1;
const NMCAP: usize = SUN_LEN - MAX_RUN_USER;

/// Writes a `/run/user/<uid>/` path with the given socket name into the address buffer,
/// [escaping](escape_nuls) interior nuls.
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
fn write_run_user(addr: &mut UdAddr, name: &[u8]) -> io::Result<()> {
    // Comparing without regard for the length of the /run/user path
    // improves robustness of programs by preventing reliance on the
    // UID being small
    if name.len() > NMCAP {
        return Err(name_too_long());
    }
    addr.reset_len();
    // SAFETY: proof by look at it
    let start = unsafe { assume_nonzero_slice(b"/run/user/") };
    // SAFETY: bounds check just up above
    unsafe { addr.push_slice(start) };
    let uid_len = {
        // SAFETY: we do not write MaybeUninit::uninit
        let buf = unsafe { addr.path_buf_mut() };
        let mut idx = start.len();
        // SAFETY: always safe
        let mut uid = unsafe { libc::getuid() };
        loop {
            buf[idx] = MaybeUninit::new((uid % 10) as u8 + b'0');
            uid /= 10;
            idx += 1;
            if uid == 0 {
                break;
            }
        }
        buf[idx] = MaybeUninit::new(b'/');
        buf[start.len()..idx].reverse();
        idx + 1 - start.len()
    };
    // SAFETY: we just wrote this many bytes to the buffer
    unsafe { addr.incr_len(uid_len) };
    let esc_start = addr.len();
    // SAFETY: bounds check at the beginning
    unsafe { addr.push_slice_with_nuls(name) };
    escape_nuls(&mut addr.path_mut()[esc_start..]);
    Ok(())
}

/// Writes the given socket name with the given prefix into the address buffer,
/// [escaping](escape_nuls) interior nuls in the name but disallowing them in the prefix.
#[allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]
fn write_prefixed(addr: &mut UdAddr, pfx: &[NonZeroU8], name: &[u8]) -> io::Result<()> {
    if pfx.len() + name.len() > SUN_LEN {
        return Err(name_too_long());
    }
    let name = check_no_nul(name)?;
    addr.reset_len();
    // SAFETY: bounds check up above
    unsafe { addr.push_slice(pfx) };
    unsafe { addr.push_slice(name) };
    escape_nuls(&mut addr.path_mut()[pfx.len()..]);
    Ok(())
}

/// Escapes the nuls in the given slice with underscores.
///
/// This exists only to retain behavior from Interprocess 2.2 and earlier.
fn escape_nuls(b: &mut [u8]) { b.iter_mut().filter(|c| **c == 0).for_each(|c| *c = b'_'); }

/// Returns `true` if the given error represents a failure that calls for continuation of
/// traversal of possible directories, `false` otherwise.
fn fail_is_benign(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(e.kind(), NotFound | Unsupported) || e.raw_os_error() == Some(libc::ENOTDIR)
}

/// Returns a nul-terminated path to the world-writable temporary directory.
fn tmpdir<'p>() -> &'p [NonZeroU8] {
    if cfg!(target_os = "android") {
        // SAFETY: there is a nul terminator
        let mut ptr = unsafe { libc::getenv(b"TMPDIR\0".as_ptr().cast()) };
        if ptr.is_null() {
            // SAFETY: there is a nul terminator
            ptr = unsafe { libc::getenv(b"TEMPDIR\0".as_ptr().cast()) };
        }
        if ptr.is_null() {
            // SAFETY: proof by look at it
            return unsafe { assume_nonzero_slice(b"/data/local/tmp/") };
        }
        // SAFETY: we got it from getenv and it's not null
        let len = unsafe { libc::strlen(ptr) };
        // SAFETY: there are no interior nuls as per strlen, and the pointer
        // is from getenv (meaning it would only be invalidated by setenv,
        // which would be a race)
        unsafe { std::slice::from_raw_parts(ptr.cast(), len) }
    } else {
        // SAFETY: proof by look at it
        unsafe { assume_nonzero_slice(b"/tmp/") }
    }
}
