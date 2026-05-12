use {
    super::{libc_wrappers::*, *},
    libc::{
        dev_t, gid_t, ino_t, mode_t, off_t, pid_t, sockaddr_storage, sockaddr_un, socklen_t,
        time_t, uid_t,
    },
    std::{
        ffi::CString,
        fmt::{self, Display, Formatter, Write as _},
        io,
        mem::ManuallyDrop,
        os::unix::{
            ffi::OsStrExt as _,
            net::{UnixListener, UnixStream},
            prelude::*,
        },
        path::{Path, PathBuf},
    },
};

pub(super) fn main() {
    print_bitwidths(&bitwidths!(
        socklen_t, dev_t, ino_t, mode_t, pid_t, uid_t, gid_t, off_t, time_t
    ));
    print_sizes(&sizes!(sockaddr_storage, sockaddr_un));
    println!();

    if geteuid() == 0 {
        println!("Dropping superuser privileges by setting effective user ID to 1");
        let _ = seteuid(1).report_error("Failed to set effective user ID to 1");
        println!();
    }

    if let Some(tmpdir) = select_tmpdir() {
        #[rustfmt::skip] println!("\
Will now perform experiments that involve the filesystem, please stand clear \
of the chosen temporary directory so as to avoid interfering with the \
experiments\n");
        requires_tmpdir(tmpdir);
    } else {
        println!("Skipping everything that requires a temporary directory\n");
    }
}

fn requires_tmpdir(tmpdir: &'static Path) {
    let listen_path = ManuallyDrop::new(tmpdir.join("interprocess-inspect-platform.sock"));
    let try_listener_c = |mask: Option<mode_t>, pre_fchmod_mode, has_post_fchmod: bool| {
        let orig_mode = match (mask, pre_fchmod_mode) {
            (_, Some(mode)) => mode,
            (Some(mask), None) => (!mask) & 0o777,
            (None, None) => 0o777,
        };
        if let Ok(listener) = listen(&listen_path, mask, pre_fchmod_mode) {
            inspect_listener(&listen_path, listener, orig_mode, has_post_fchmod);
        }
        println!();
    };
    try_listener_c(Some(0o000), None, true);
    try_listener_c(Some(0o777), None, false);
    try_listener_c(None, Some(0o000), false);
    try_listener_c(Some(0o0222), None, false);
    try_listener_c(Some(0o0333), None, false);
    try_listener_c(Some(0o0444), None, false);
    try_listener_c(Some(0o0555), None, false);
    let _ = std::fs::remove_file(&*listen_path).report_error("Failed to remove socket");
}

fn listen(
    path: &Path,
    mask: Option<mode_t>,
    fchmod_mode: Option<mode_t>,
) -> io::Result<UnixListener> {
    let can_overwrite =
        access(&CString::new(path.as_os_str().as_bytes()).unwrap(), false, false, false).is_ok();
    let _guard = mask.map(umask);

    let pathdisp = path.display();

    // FUTURE use format_args!
    let mut bindinfo = String::with_capacity(128);
    if let Some(mask) = mask {
        let _ = write!(bindinfo, " with umask {mask:0>3o}");
    }
    if let Some(mode) = fchmod_mode {
        let _ = write!(bindinfo, " with pre-bind fchmod to {mode:0>4o}");
    }
    let _ = write!(bindinfo, " to {pathdisp}");
    let err = format!("Failed to bind{bindinfo}");

    let bind = || {
        let mut report_err = true;
        let rslt = if let Some(mode) = fchmod_mode {
            bind_with_hook(path, |fd| {
                fchmod(fd, mode)
                    .report_error("Failed to fchmod listener before bind")
                    .set_if_error(&mut report_err, false)
            })
        } else {
            UnixListener::bind(path)
        };
        if let Err(e) = &rslt {
            if matches!(e.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::AlreadyExists) {
                report_err = false;
            }
        }
        rslt.report_error_if(report_err, &err)
    };
    let success = |caution: bool| {
        display_fn(move |f| {
            write!(f, "{}Successfully bound listener{bindinfo}", val_if(caution, "[Caution] "))
        })
    };

    match bind() {
        Ok(listener) => {
            let warn = val_if(can_overwrite, " - previous socket file was overwritten");
            println!("{}{warn}", success(can_overwrite));
            Ok(listener)
        }
        Err(e) if matches!(e.kind(), io::ErrorKind::AddrInUse | io::ErrorKind::AlreadyExists) => {
            std::fs::remove_file(path).report_error("Failed to remove stale socket")?;
            let rslt = bind();
            if rslt.is_ok() {
                println!("{} after unlinking previous socket", success(false));
            }
            rslt
        }
        Err(e) => Err(e),
    }
}

fn inspect_listener(
    listen_path: &Path,
    listener: UnixListener,
    orig_mode: mode_t,
    has_post_fchmod: bool,
) {
    let path_cstring = CString::new(listen_path.as_os_str().as_bytes().to_owned()).unwrap();
    let chk_stat = |fchmodded: bool| {
        let fchmodded = if fchmodded { " (fchmodded)" } else { "" };
        if let Ok(stat) = fstat(listener.as_fd()).report_error("Failed to fstat listener") {
            let stat = StatDisplay(stat);
            println!("Listener fstat{fchmodded}: {stat}");
        }
        if let Ok(stat) = stat(&path_cstring).report_error("Failed to stat listener") {
            let stat = StatDisplay(stat);
            println!("Listener  stat{fchmodded}: {stat}");
        }
    };
    chk_stat(false);

    if UnixStream::connect(listen_path).report_error("Failed to connect to listener").is_ok() {
        let is_prohibited = orig_mode & 0b010_000_000 == 0;
        let (caution, prohib) = if is_prohibited {
            ("[Caution] ", " - privilege checking on socket files presumably inoperative")
        } else {
            ("", "")
        };
        println!("{caution}Successfully connected to listener{prohib}");
    }

    if has_post_fchmod
        && fchmod(listener.as_fd(), 0)
            .report_error("Failed to fchmod listener after bind")
            .is_ok()
    {
        println!("Post-bind change of listener mode to 000 returned success");
        chk_stat(true);

        if UnixStream::connect(listen_path)
            .report_error("Failed to connect to listener after post-bind fchmod")
            .is_ok()
        {
            #[rustfmt::skip] println!("\
[Caution] Successfully connected to listener after post-bind fchmod to 000 - \
it is likely that the mode set by fchmod only applies when creating the \
socket file during bind()"
                );
        }
    }
}

const DEFAULT_TMPDIR: &str = if cfg!(target_os = "android") { "/data/local/tmp" } else { "/tmp" };
fn select_tmpdir() -> Option<&'static Path> {
    fn valdisp<'a>(var: Option<&'a str>, val: &'a Path) -> impl Display + 'a {
        display_fn(move |f| {
            if let Some(var) = var {
                write!(f, "{var} = ")?;
            }
            write!(f, "{}", val.display())
        })
    }
    fn try_var(var: Option<&str>) -> Option<(Option<&str>, &'static Path)> {
        let val = if let Some(var) = var {
            let Some(val) = std::env::var_os(var) else {
                println!("{var:<8} is unset");
                return None;
            };
            Box::leak(PathBuf::from(val).into_boxed_path())
        } else {
            Path::new(DEFAULT_TMPDIR)
        };
        let valdisp = valdisp(var, val);
        let md = std::fs::metadata(val)
            .report_error_args(format_args!(
                "Could not check {valdisp} for whether it is a directory or not"
            ))
            .ok()?;
        if !md.file_type().is_dir() {
            println!("{valdisp} is not a directory");
            return None;
        }
        Some((var, val))
    }
    let rslt = try_var(Some("TMPDIR"))
        .or_else(|| try_var(Some("TEMPDIR")))
        .or_else(|| try_var(Some("TMP")))
        .or_else(|| try_var(Some("TEMP")))
        .or_else(|| try_var(None))
        .map(|(var, val)| {
            println!("Using temporary directory {}", valdisp(var, val));
            val
        });
    println!();
    rslt
}

pub struct StatDisplay(libc::stat);
impl Display for StatDisplay {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let &StatDisplay(libc::stat { st_dev, st_ino, st_mode, st_uid, st_gid, .. }) = self;
        let (majdev, mindev) = (libc::major(st_dev), libc::minor(st_dev));
        let (devw, inow) = (c_int::MAX.ilog10() as usize + 1, ino_t::MAX.ilog10() as usize + 1);
        let (uidw, gidw) = (uid_t::MAX.ilog10() as usize + 1, gid_t::MAX.ilog10() as usize + 1);
        write!(
            f,
            "[dev {majdev:>devw$},{mindev:0devw$}] [ino {st_ino:>inow$}] \
[mode {st_mode:0>6o}] [uid {st_uid:>uidw$}] [gid {st_gid:>gidw$}]"
        )
    }
}
