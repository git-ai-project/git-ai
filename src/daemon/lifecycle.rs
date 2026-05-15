use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    LockHeld,
    AlreadyRunning(u32),
    ForkFailed,
    Generic(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "daemon I/O error: {}", e),
            Error::LockHeld => write!(f, "daemon lock already held"),
            Error::AlreadyRunning(pid) => write!(f, "daemon already running (pid {})", pid),
            Error::ForkFailed => write!(f, "fork failed"),
            Error::Generic(msg) => write!(f, "daemon error: {}", msg),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct DaemonPid {
    pub pid: u32,
    pub started_at: String,
    pub version: String,
}

pub struct DaemonPaths {
    pub base_dir: PathBuf,
    pub lock_file: PathBuf,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub trace2_sock: PathBuf,
    pub control_sock: PathBuf,
}

impl DaemonPaths {
    pub fn resolve() -> Self {
        let base_dir = Self::base_dir();

        let lock_file = base_dir.join("daemon.lock");
        let pid_file = base_dir.join("daemon.pid.json");
        let log_file = base_dir.join("daemon.log");

        let trace2_sock = Self::resolve_socket_path(&base_dir, "trace2");
        let control_sock = Self::resolve_socket_path(&base_dir, "control");

        DaemonPaths {
            base_dir,
            lock_file,
            pid_file,
            log_file,
            trace2_sock,
            control_sock,
        }
    }

    pub fn base_dir() -> PathBuf {
        #[cfg(unix)]
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        #[cfg(windows)]
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("APPDATA"))
            .unwrap_or_else(|_| "C:\\Temp".to_string());

        PathBuf::from(&home)
            .join(".git-ai")
            .join("internal")
            .join("daemon")
    }

    fn resolve_socket_path(base_dir: &Path, name: &str) -> PathBuf {
        #[cfg(unix)]
        {
            let candidate = base_dir.join(format!("{}.sock", name));
            let candidate_str = candidate.to_string_lossy();

            if candidate_str.len() >= 100 {
                let mut hasher = Sha256::new();
                hasher.update(base_dir.to_string_lossy().as_bytes());
                let hash = hasher.finalize();
                let short_hash = hex_encode(&hash[..8]);
                let dir = PathBuf::from(format!("/tmp/git-ai-d-{}", short_hash));
                dir.join(format!("{}.sock", name))
            } else {
                candidate
            }
        }
        #[cfg(windows)]
        {
            let _ = base_dir;
            let mut hasher = Sha256::new();
            hasher.update(base_dir.to_string_lossy().as_bytes());
            let hash = hasher.finalize();
            let short = hex_encode(&hash[..8]);
            PathBuf::from(format!(r"\\.\pipe\git-ai-{}-{}", short, name))
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), Error> {
        fs::create_dir_all(&self.base_dir)?;

        if let Some(parent) = self.trace2_sock.parent()
            && parent != self.base_dir
        {
            fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.control_sock.parent()
            && parent != self.base_dir
        {
            fs::create_dir_all(parent)?;
        }

        Ok(())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub fn acquire_lock(path: &Path) -> Result<File, Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret != 0 {
            return Err(Error::LockHeld);
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use std::ptr;
        let handle = file.as_raw_handle();
        let mut overlapped: WinapiOverlapped = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            LockFileEx(
                handle as _,
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if ret == 0 {
            return Err(Error::LockHeld);
        }
    }

    Ok(file)
}

#[cfg(windows)]
#[repr(C)]
struct WinapiOverlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: usize,
}
#[cfg(windows)]
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x00000002;
#[cfg(windows)]
const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x00000001;
#[cfg(windows)]
extern "system" {
    fn LockFileEx(
        hFile: *mut std::ffi::c_void,
        dwFlags: u32,
        dwReserved: u32,
        nNumberOfBytesToLockLow: u32,
        nNumberOfBytesToLockHigh: u32,
        lpOverlapped: *mut WinapiOverlapped,
    ) -> i32;
    fn GetStdHandle(nStdHandle: u32) -> *mut std::ffi::c_void;
    fn SetStdHandle(nStdHandle: u32, hHandle: *mut std::ffi::c_void) -> i32;
}
#[cfg(windows)]
const STD_ERROR_HANDLE: u32 = 0xFFFFFFF4u32; // -12 as u32

pub fn write_pid_file(path: &Path) -> Result<(), Error> {
    let pid = std::process::id();
    let started_at = iso_now();
    let version = env!("CARGO_PKG_VERSION");

    let content = format!(
        "{{\"pid\":{},\"started_at\":\"{}\",\"version\":\"{}\"}}",
        pid, started_at, version
    );

    let mut file = File::create(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub fn read_pid_file(path: &Path) -> Option<DaemonPid> {
    let content = fs::read_to_string(path).ok()?;
    let pid = extract_json_u32(&content, "pid")?;
    let started_at = extract_json_string(&content, "started_at").unwrap_or_default();
    let version = extract_json_string(&content, "version").unwrap_or_default();
    Some(DaemonPid {
        pid,
        started_at,
        version,
    })
}

fn extract_json_u32(json: &str, key: &str) -> Option<u32> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let after = &json[idx + pattern.len()..];
    let after = after.trim_start();
    let end = after.find(|c: char| !c.is_ascii_digit())?;
    after[..end].parse().ok()
}

fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":\"", key);
    let idx = json.find(&pattern)?;
    let after = &json[idx + pattern.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

pub fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as i32, 0) };
        ret == 0
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
pub fn daemonize() -> Result<(), Error> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(Error::ForkFailed);
    }
    if pid > 0 {
        std::process::exit(0);
    }

    if unsafe { libc::setsid() } < 0 {
        return Err(Error::Generic("setsid failed".to_string()));
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(Error::ForkFailed);
    }
    if pid > 0 {
        std::process::exit(0);
    }

    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, 0);
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            if devnull > 2 {
                libc::close(devnull);
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x00000008;

#[cfg(windows)]
#[repr(C)]
struct STARTUPINFOW {
    cb: u32,
    _reserved: *mut u16,
    _desktop: *mut u16,
    _title: *mut u16,
    _dw_x: u32,
    _dw_y: u32,
    _dw_x_size: u32,
    _dw_y_size: u32,
    _dw_x_count_chars: u32,
    _dw_y_count_chars: u32,
    _dw_fill_attribute: u32,
    _dw_flags: u32,
    _w_show_window: u16,
    _cb_reserved2: u16,
    _lp_reserved2: *mut u8,
    _h_std_input: *mut std::ffi::c_void,
    _h_std_output: *mut std::ffi::c_void,
    _h_std_error: *mut std::ffi::c_void,
}

#[cfg(windows)]
#[repr(C)]
struct PROCESS_INFORMATION {
    h_process: *mut std::ffi::c_void,
    h_thread: *mut std::ffi::c_void,
    dw_process_id: u32,
    dw_thread_id: u32,
}

#[cfg(windows)]
extern "system" {
    fn CreateProcessW(
        lpApplicationName: *const u16,
        lpCommandLine: *mut u16,
        lpProcessAttributes: *mut std::ffi::c_void,
        lpThreadAttributes: *mut std::ffi::c_void,
        bInheritHandles: i32,
        dwCreationFlags: u32,
        lpEnvironment: *mut std::ffi::c_void,
        lpCurrentDirectory: *const u16,
        lpStartupInfo: *mut STARTUPINFOW,
        lpProcessInformation: *mut PROCESS_INFORMATION,
    ) -> i32;

    fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
pub fn daemonize_windows() -> Result<(), Error> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::Generic(format!("failed to get current exe path: {}", e)))?;

    let exe_str = exe.to_string_lossy();
    let cmd_line = format!("\"{}\" bg run", exe_str);

    // Encode command line as wide string (UTF-16) with null terminator
    let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().collect();
    cmd_wide.push(0);

    let mut startup_info: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup_info.cb = std::mem::size_of::<STARTUPINFOW>() as u32;

    let mut proc_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let success = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd_wide.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0, // don't inherit handles
            CREATE_NO_WINDOW | DETACHED_PROCESS,
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut startup_info,
            &mut proc_info,
        )
    };

    if success == 0 {
        return Err(Error::Generic(
            "CreateProcessW failed to launch daemon child".to_string(),
        ));
    }

    // Close the handles we don't need in the parent
    unsafe {
        CloseHandle(proc_info.h_process);
        CloseHandle(proc_info.h_thread);
    }

    // Parent exits successfully — the child is now a detached daemon
    std::process::exit(0);
}

pub fn redirect_stderr_to_log(log_path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::io::IntoRawFd;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;

        let fd = file.into_raw_fd();
        unsafe {
            libc::dup2(fd, 2);
            if fd != 2 {
                libc::close(fd);
            }
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;

        let handle = file.as_raw_handle();
        unsafe {
            let stderr_handle = GetStdHandle(STD_ERROR_HANDLE);
            SetStdHandle(STD_ERROR_HANDLE, handle as _);
            CloseHandle(stderr_handle);
        }
        std::mem::forget(file); // Keep file open for the lifetime of the process
    }

    #[cfg(not(any(unix, windows)))]
    {
        eprintln!(
            "[git-ai] warning: stderr redirect not supported on this platform, logs would be at {}",
            log_path.display()
        );
    }

    Ok(())
}

#[cfg(unix)]
static SIGNAL_SHUTDOWN_FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

#[cfg(windows)]
static CTRL_SHUTDOWN_FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

#[cfg(windows)]
const CTRL_C_EVENT: u32 = 0;
#[cfg(windows)]
const CTRL_CLOSE_EVENT: u32 = 2;

#[cfg(windows)]
extern "system" {
    fn SetConsoleCtrlHandler(
        HandlerRoutine: Option<unsafe extern "system" fn(u32) -> i32>,
        Add: i32,
    ) -> i32;
}

#[cfg(windows)]
unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_CLOSE_EVENT {
        if let Some(flag) = CTRL_SHUTDOWN_FLAG.get() {
            flag.store(true, Ordering::Relaxed);
        }
        1 // TRUE — we handled it
    } else {
        0 // FALSE — pass to next handler
    }
}

pub fn install_signal_handlers(shutdown: Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        SIGNAL_SHUTDOWN_FLAG.get_or_init(|| shutdown);

        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = signal_handler as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
        }

        extern "C" fn signal_handler(_sig: libc::c_int) {
            if let Some(flag) = SIGNAL_SHUTDOWN_FLAG.get() {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }

    #[cfg(windows)]
    {
        CTRL_SHUTDOWN_FLAG.get_or_init(|| shutdown);

        unsafe {
            SetConsoleCtrlHandler(Some(ctrl_handler), 1);
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = shutdown;
    }
}

pub fn kill_v1_daemon() {
    let pid_path = DaemonPaths::base_dir().join("daemon.pid.json");

    let daemon_pid = match read_pid_file(&pid_path) {
        Some(p) => p,
        None => return,
    };

    if !is_process_alive(daemon_pid.pid) {
        let _ = fs::remove_file(&pid_path);
        return;
    }

    eprintln!("[git-ai] stopping v1 daemon (pid {})...", daemon_pid.pid);
    terminate_process(daemon_pid.pid);

    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !is_process_alive(daemon_pid.pid) {
            let _ = fs::remove_file(&pid_path);
            return;
        }
    }

    eprintln!(
        "[git-ai] warning: v1 daemon (pid {}) did not exit within 5s",
        daemon_pid.pid
    );
}

pub fn terminate_process(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    #[cfg(windows)]
    {
        use std::process::Command;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn iso_now() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let mins = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, mins, s
    )
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
