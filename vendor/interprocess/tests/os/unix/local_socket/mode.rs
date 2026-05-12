use {
    crate::{
        local_socket::{traits::Stream as _, Listener, ListenerOptions, Name, NameInner, Stream},
        os::unix::local_socket::ListenerOptionsExt,
        tests::util::*,
        OrErrno,
    },
    libc::mode_t,
    std::{
        ffi::{CString, OsStr},
        io,
        mem::zeroed,
        os::unix::prelude::*,
    },
};

fn get_file_mode(fname: &OsStr) -> TestResult<mode_t> {
    let mut cfname = fname.as_bytes().to_owned();
    cfname.push(0);
    let fname = CString::from_vec_with_nul(cfname)?;
    let mut stat = unsafe { zeroed::<libc::stat>() };
    unsafe { libc::stat(fname.as_ptr(), &mut stat) != -1 }
        .true_val_or_errno(())
        .opname("stat")?;
    Ok(stat.st_mode & 0o777)
}

fn get_fd_mode(fd: BorrowedFd<'_>) -> TestResult<mode_t> {
    let mut stat = unsafe { zeroed::<libc::stat>() };
    unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) != -1 }
        .true_val_or_errno(())
        .opname("stat")?;
    Ok(stat.st_mode & 0o777)
}

fn test_inner(path: bool) -> TestResult {
    const MODE: libc::mode_t = 0o600;
    let (name, listener) =
        listen_and_pick_name(&mut namegen_local_socket(make_id!(), path), |nm| {
            let rslt =
                ListenerOptions::new().name(nm.borrow()).mode(MODE).create_sync().map(Some);
            if cfg!(not(any(target_os = "linux", target_os = "android", target_os = "freebsd"))) {
                return if rslt.err().filter(|e| e.kind() == io::ErrorKind::Unsupported).is_some()
                {
                    Ok(None)
                } else {
                    Err(io::Error::other("unexpected success, please update this test"))
                };
            }
            rslt
        })?;

    // If listener is None, we're on a platform on which we expect this to not be supported
    let Some(listener) = listener else { return Ok(()) };

    let _ = Stream::connect(name.borrow()).opname("client connect")?;
    let actual_mode = if let Name(NameInner::UdSocketPath(path)) = name {
        get_file_mode(&path)
    } else {
        let fd = match &listener {
            Listener::UdSocket(l) => l.as_fd(),
        };
        get_fd_mode(fd)
    }
    .opname("get mode")?;
    if actual_mode != 0 {
        // FreeBSD 14.2 and below refuses to fstat sockets
        // for reasons I cannot even begin to fathom
        ensure_eq!(actual_mode, MODE);
    }

    Ok(())
}

#[test]
fn file_main() -> TestResult { test_wrapper(|| test_inner(true)) }

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn namespaced_main() -> TestResult { test_wrapper(|| test_inner(false)) }
