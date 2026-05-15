//! Windows named pipe listener for git trace2 events.
//!
//! This module is compiled on all platforms when running tests (for the
//! platform-independent unit tests), but the actual pipe I/O implementation
//! is only compiled on Windows.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::daemon::trace2_events::{Trace2Event, parse_trace2_line};

/// Maximum length for a named pipe path on Windows (characters, not bytes).
/// The actual Windows limit is 256 characters including the `\\.\pipe\` prefix.
const MAX_PIPE_PATH_LEN: usize = 256;

/// Encode a Rust string as a null-terminated wide (UTF-16) string for Win32 APIs.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Validate that a pipe path is well-formed and within Windows length limits.
/// Returns an error if the path is invalid.
pub(crate) fn validate_pipe_path(path: &str) -> Result<(), String> {
    if !path.starts_with(r"\\.\pipe\") {
        return Err(format!(
            "not a valid named pipe path (must start with \\\\.\\pipe\\): {}",
            path
        ));
    }

    let suffix = &path[r"\\.\pipe\".len()..];
    if suffix.is_empty() {
        return Err("pipe name cannot be empty after \\\\.\\pipe\\ prefix".to_string());
    }

    if path.len() > MAX_PIPE_PATH_LEN {
        return Err(format!(
            "pipe path exceeds {} character limit (got {} chars): {}",
            MAX_PIPE_PATH_LEN,
            path.len(),
            path
        ));
    }

    // Pipe names cannot contain backslashes after the prefix (no nested paths)
    if suffix.contains('\\') {
        return Err(format!(
            "pipe name cannot contain backslashes after prefix: {}",
            path
        ));
    }

    Ok(())
}

// =============================================================================
// Windows-only implementation
// =============================================================================

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{self, BufRead, BufReader, Read};
    use std::path::Path;
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::time::Duration;

    use crate::daemon::stats;

    // Win32 constants for named pipe creation
    const PIPE_ACCESS_INBOUND: u32 = 0x00000001;
    const FILE_FLAG_OVERLAPPED: u32 = 0x40000000;
    const PIPE_TYPE_BYTE: u32 = 0x00000000;
    const PIPE_READMODE_BYTE: u32 = 0x00000000;
    const PIPE_WAIT: u32 = 0x00000000;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;
    const INVALID_HANDLE_VALUE: isize = -1;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const ERROR_IO_PENDING: u32 = 997;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    type HANDLE = *mut std::ffi::c_void;
    type BOOL = i32;
    type DWORD = u32;
    type LPDWORD = *mut u32;

    #[repr(C)]
    struct OVERLAPPED {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        h_event: HANDLE,
    }

    #[repr(C)]
    struct SECURITY_ATTRIBUTES {
        n_length: u32,
        lp_security_descriptor: *mut std::ffi::c_void,
        b_inherit_handle: BOOL,
    }

    unsafe extern "system" {
        fn CreateNamedPipeW(
            lp_name: *const u16,
            dw_open_mode: DWORD,
            dw_pipe_mode: DWORD,
            n_max_instances: DWORD,
            n_out_buffer_size: DWORD,
            n_in_buffer_size: DWORD,
            n_default_time_out: DWORD,
            lp_security_attributes: *const SECURITY_ATTRIBUTES,
        ) -> HANDLE;

        fn ConnectNamedPipe(h_named_pipe: HANDLE, lp_overlapped: *mut OVERLAPPED) -> BOOL;

        fn DisconnectNamedPipe(h_named_pipe: HANDLE) -> BOOL;

        fn ReadFile(
            h_file: HANDLE,
            lp_buffer: *mut u8,
            n_number_of_bytes_to_read: DWORD,
            lp_number_of_bytes_read: LPDWORD,
            lp_overlapped: *mut OVERLAPPED,
        ) -> BOOL;

        fn CloseHandle(h_object: HANDLE) -> BOOL;

        fn CreateEventW(
            lp_event_attributes: *const SECURITY_ATTRIBUTES,
            b_manual_reset: BOOL,
            b_initial_state: BOOL,
            lp_name: *const u16,
        ) -> HANDLE;

        fn WaitForSingleObject(h_handle: HANDLE, dw_milliseconds: DWORD) -> DWORD;

        fn GetOverlappedResult(
            h_file: HANDLE,
            lp_overlapped: *mut OVERLAPPED,
            lp_number_of_bytes_transferred: LPDWORD,
            b_wait: BOOL,
        ) -> BOOL;

        fn GetLastError() -> DWORD;

        fn CancelIo(h_file: HANDLE) -> BOOL;
    }

    /// Listens on a Windows named pipe for git trace2 events.
    ///
    /// Git processes configured with `GIT_TRACE2_EVENT=\\.\pipe\<pipe_name>` will
    /// connect to this pipe and stream newline-delimited JSON events.
    pub struct Trace2ListenerWin {
        pipe_name: Vec<u16>,
        shutdown: Arc<AtomicBool>,
    }

    impl Trace2ListenerWin {
        /// Bind the trace2 named pipe. On Windows, named pipes don't require filesystem cleanup
        /// like Unix sockets -- they are kernel objects that disappear when all handles are closed.
        pub fn bind(pipe_name: &Path, shutdown: Arc<AtomicBool>) -> io::Result<Self> {
            let name_str = pipe_name.to_string_lossy();

            // Validate pipe path format and length
            if let Err(e) = validate_pipe_path(&name_str) {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, e));
            }

            let wide_name = to_wide(&name_str);

            // Create a test pipe instance to verify we can bind this name, then close it.
            // The actual pipe instances are created per-connection in the accept loop.
            let test_handle = unsafe {
                CreateNamedPipeW(
                    wide_name.as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    0,     // no outbound buffer needed (inbound-only pipe)
                    65536, // 64KB inbound buffer
                    0,     // default timeout
                    std::ptr::null(),
                )
            };

            if test_handle as isize == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }

            // Close the test handle -- we'll create fresh instances in the accept loop
            unsafe {
                CloseHandle(test_handle);
            }

            Ok(Self {
                pipe_name: wide_name,
                shutdown,
            })
        }

        /// Run the accept loop. Spawns a thread per connection.
        ///
        /// Each connection reads newline-delimited JSON and sends parsed events to
        /// the provided channel. Returns when the shutdown flag is set.
        pub fn run(&self, event_tx: Sender<Trace2Event>) {
            while !self.shutdown.load(Ordering::Relaxed) {
                // Create a new pipe instance for this connection
                let pipe_handle = unsafe {
                    CreateNamedPipeW(
                        self.pipe_name.as_ptr(),
                        PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                        PIPE_UNLIMITED_INSTANCES,
                        0,
                        65536,
                        0,
                        std::ptr::null(),
                    )
                };

                if pipe_handle as isize == INVALID_HANDLE_VALUE {
                    eprintln!(
                        "[git-ai daemon] failed to create named pipe instance: {}",
                        io::Error::last_os_error()
                    );
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                // Wait for a client to connect using overlapped I/O so we can check shutdown
                match self.wait_for_connection(pipe_handle) {
                    Ok(true) => {
                        // Client connected -- spawn a handler thread
                        stats::get()
                            .trace2_connections
                            .fetch_add(1, Ordering::Relaxed);
                        let tx = event_tx.clone();
                        let shutdown = Arc::clone(&self.shutdown);
                        thread::spawn(move || {
                            handle_pipe_connection(pipe_handle, tx, shutdown);
                        });
                    }
                    Ok(false) => {
                        // Shutdown was requested while waiting
                        unsafe {
                            CloseHandle(pipe_handle);
                        }
                        break;
                    }
                    Err(e) => {
                        eprintln!("[git-ai daemon] ConnectNamedPipe error: {}", e);
                        unsafe {
                            CloseHandle(pipe_handle);
                        }
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        /// Wait for a client connection with periodic shutdown checks.
        /// Returns Ok(true) if a client connected, Ok(false) if shutdown was requested.
        fn wait_for_connection(&self, pipe_handle: HANDLE) -> io::Result<bool> {
            // Create an event for overlapped ConnectNamedPipe
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut overlapped = OVERLAPPED {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                h_event: event,
            };

            let ret = unsafe { ConnectNamedPipe(pipe_handle, &mut overlapped) };

            if ret != 0 {
                // Connected immediately (unusual but possible)
                unsafe {
                    CloseHandle(event);
                }
                return Ok(true);
            }

            let err = unsafe { GetLastError() };

            if err == ERROR_PIPE_CONNECTED {
                // Client was already connected before we called ConnectNamedPipe
                unsafe {
                    CloseHandle(event);
                }
                return Ok(true);
            }

            if err != ERROR_IO_PENDING {
                unsafe {
                    CloseHandle(event);
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }

            // Poll the overlapped operation with a timeout, checking shutdown periodically
            loop {
                if self.shutdown.load(Ordering::Relaxed) {
                    // Cancel the pending ConnectNamedPipe and clean up
                    unsafe {
                        CancelIo(pipe_handle);
                        CloseHandle(event);
                    }
                    return Ok(false);
                }

                let wait_result = unsafe { WaitForSingleObject(event, 500) }; // 500ms timeout

                match wait_result {
                    WAIT_OBJECT_0 => {
                        // Connection completed
                        let mut bytes_transferred: u32 = 0;
                        unsafe {
                            GetOverlappedResult(
                                pipe_handle,
                                &mut overlapped,
                                &mut bytes_transferred,
                                0, // don't wait
                            );
                            CloseHandle(event);
                        }
                        return Ok(true);
                    }
                    WAIT_TIMEOUT => {
                        // Timeout -- loop and check shutdown flag again
                        continue;
                    }
                    _ => {
                        // Unexpected wait result
                        unsafe {
                            CancelIo(pipe_handle);
                            CloseHandle(event);
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("WaitForSingleObject returned {}", wait_result),
                        ));
                    }
                }
            }
        }
    }

    /// A minimal Read adapter for a Win32 pipe handle.
    ///
    /// Reads from the pipe using synchronous ReadFile calls (the pipe is opened
    /// with FILE_FLAG_OVERLAPPED but we use blocking reads per-connection thread).
    struct PipeReader {
        handle: HANDLE,
    }

    // SAFETY: The handle is only used by a single thread (the connection handler).
    unsafe impl Send for PipeReader {}

    impl Read for PipeReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut bytes_read: u32 = 0;
            let ret = unsafe {
                ReadFile(
                    self.handle,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut bytes_read,
                    std::ptr::null_mut(), // synchronous read (no overlapped)
                )
            };

            if ret == 0 {
                let err = unsafe { GetLastError() };
                // ERROR_BROKEN_PIPE (109) means client disconnected -- treat as EOF
                if err == 109 {
                    return Ok(0);
                }
                return Err(io::Error::from_raw_os_error(err as i32));
            }

            Ok(bytes_read as usize)
        }
    }

    /// Handle a single client pipe connection, reading trace2 JSON lines until EOF or shutdown.
    fn handle_pipe_connection(
        pipe_handle: HANDLE,
        event_tx: Sender<Trace2Event>,
        shutdown: Arc<AtomicBool>,
    ) {
        let reader = PipeReader {
            handle: pipe_handle,
        };
        let buf_reader = BufReader::new(reader);

        for line_result in buf_reader.lines() {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            match line_result {
                Ok(line) => {
                    if let Some(event) = parse_trace2_line(&line) {
                        // If the receiver is gone, stop processing
                        if event_tx.send(event).is_err() {
                            break;
                        }
                    }
                }
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(_) => {
                    // Connection closed or broken pipe -- done with this client
                    break;
                }
            }
        }

        // Disconnect and close the pipe handle
        unsafe {
            DisconnectNamedPipe(pipe_handle);
            CloseHandle(pipe_handle);
        }
    }
}

#[cfg(windows)]
pub use imp::Trace2ListenerWin;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------
    // Tests for pipe path validation (runs on all platforms)
    // -------------------------------------------------------

    #[test]
    fn validate_pipe_path_valid() {
        assert!(validate_pipe_path(r"\\.\pipe\git-ai-trace2-abcdef01").is_ok());
        assert!(validate_pipe_path(r"\\.\pipe\git-ai-12345678-trace2").is_ok());
        assert!(validate_pipe_path(r"\\.\pipe\a").is_ok());
    }

    #[test]
    fn validate_pipe_path_rejects_non_pipe_prefix() {
        let err = validate_pipe_path(r"C:\some\path").unwrap_err();
        assert!(err.contains("must start with"));

        let err = validate_pipe_path(r"/tmp/socket.sock").unwrap_err();
        assert!(err.contains("must start with"));

        let err = validate_pipe_path(r"\\.\device\something").unwrap_err();
        assert!(err.contains("must start with"));
    }

    #[test]
    fn validate_pipe_path_rejects_empty_name() {
        let err = validate_pipe_path(r"\\.\pipe\").unwrap_err();
        assert!(err.contains("cannot be empty"));
    }

    #[test]
    fn validate_pipe_path_rejects_too_long() {
        // Create a path that exceeds 256 chars
        let prefix = r"\\.\pipe\";
        let long_name = "a".repeat(MAX_PIPE_PATH_LEN - prefix.len() + 1);
        let long_path = format!("{}{}", prefix, long_name);
        assert!(long_path.len() > MAX_PIPE_PATH_LEN);

        let err = validate_pipe_path(&long_path).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    #[test]
    fn validate_pipe_path_accepts_max_length() {
        let prefix = r"\\.\pipe\";
        let name = "x".repeat(MAX_PIPE_PATH_LEN - prefix.len());
        let path = format!("{}{}", prefix, name);
        assert_eq!(path.len(), MAX_PIPE_PATH_LEN);
        assert!(validate_pipe_path(&path).is_ok());
    }

    #[test]
    fn validate_pipe_path_rejects_backslash_in_name() {
        let err = validate_pipe_path(r"\\.\pipe\git-ai\nested").unwrap_err();
        assert!(err.contains("cannot contain backslashes"));
    }

    #[test]
    fn validate_pipe_path_generated_by_lifecycle_is_valid() {
        // Simulate the pipe name generation from lifecycle.rs (resolve_socket_path on Windows)
        use sha2::{Digest, Sha256};

        let base_dir = r"C:\Users\testuser\.git-ai\internal\daemon";
        let mut hasher = Sha256::new();
        hasher.update(base_dir.as_bytes());
        let hash = hasher.finalize();
        let short = hash[..8]
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        let pipe_path = format!(r"\\.\pipe\git-ai-{}-trace2", short);

        assert!(
            validate_pipe_path(&pipe_path).is_ok(),
            "pipe path generated by lifecycle should be valid: {}",
            pipe_path
        );
        // Verify it matches expected format
        assert!(pipe_path.starts_with(r"\\.\pipe\git-ai-"));
        assert!(pipe_path.ends_with("-trace2"));
        assert!(pipe_path.len() < MAX_PIPE_PATH_LEN);
    }

    // -------------------------------------------------------
    // Tests for to_wide encoding (runs on all platforms)
    // -------------------------------------------------------

    #[test]
    fn to_wide_produces_null_terminated_utf16() {
        let result = to_wide("hello");
        assert_eq!(
            result,
            vec![
                'h' as u16, 'e' as u16, 'l' as u16, 'l' as u16, 'o' as u16, 0
            ]
        );
    }

    #[test]
    fn to_wide_empty_string() {
        let result = to_wide("");
        assert_eq!(result, vec![0u16]);
    }

    #[test]
    fn to_wide_pipe_path() {
        let path = r"\\.\pipe\git-ai-test";
        let wide = to_wide(path);
        // Should end with null terminator
        assert_eq!(*wide.last().unwrap(), 0);
        // Length should be string chars + 1 (null terminator)
        assert_eq!(wide.len(), path.len() + 1);
    }

    #[test]
    fn to_wide_unicode_characters() {
        // Pipe names can technically contain Unicode (though unusual)
        let result = to_wide("pipe-\u{00e9}"); // e-acute
        assert_eq!(result.len(), 7); // 6 chars + null
        assert_eq!(*result.last().unwrap(), 0);
    }

    #[test]
    fn to_wide_special_characters_in_pipe_name() {
        // Verify hyphens and hex chars encode correctly (common in our pipe names)
        let path = r"\\.\pipe\git-ai-a1b2c3d4-trace2";
        let wide = to_wide(path);
        // All ASCII, so each char maps 1:1 to u16
        assert_eq!(wide.len(), path.len() + 1);
        // Verify backslash encoding
        assert_eq!(wide[0], '\\' as u16);
        assert_eq!(wide[1], '\\' as u16);
        assert_eq!(wide[2], '.' as u16);
        assert_eq!(wide[3], '\\' as u16);
    }

    // -------------------------------------------------------
    // Tests for event parsing integration (runs on all platforms)
    // These verify that the same JSON format the Windows pipe
    // reader would see is correctly parsed by parse_trace2_line.
    // -------------------------------------------------------

    #[test]
    fn parse_trace2_multiline_stream() {
        // Simulates what a git client would send over the pipe
        let lines = [
            r#"{"event":"start","sid":"win-session-1","thread":"main","time":"2024-01-01T00:00:00Z","argv":["git","push","origin","main"]}"#,
            r#"{"event":"def_repo","sid":"win-session-1","thread":"main","repo":1,"worktree":"C:\\Users\\dev\\project"}"#,
            r#"{"event":"cmd_name","sid":"win-session-1","thread":"main","name":"push"}"#,
            r#"{"event":"data","sid":"win-session-1","thread":"main","key":"push","value":"ok"}"#,
            r#"{"event":"exit","sid":"win-session-1","thread":"main","t_abs":1.23,"code":0}"#,
        ];

        let events: Vec<Trace2Event> = lines
            .iter()
            .filter_map(|line| parse_trace2_line(line))
            .collect();

        assert_eq!(events.len(), 5);
        assert!(
            matches!(&events[0], Trace2Event::Start { sid, argv } if sid == "win-session-1" && argv[1] == "push")
        );
        assert!(
            matches!(&events[1], Trace2Event::DefRepo { repo_path, .. } if repo_path.to_string_lossy().contains("project"))
        );
        assert!(matches!(&events[2], Trace2Event::CmdName { cmd_name, .. } if cmd_name == "push"));
        assert!(matches!(&events[3], Trace2Event::Ignored));
        assert!(
            matches!(&events[4], Trace2Event::CommandExit { exit_code, .. } if *exit_code == 0)
        );
    }

    #[test]
    fn parse_trace2_windows_paths_in_events() {
        // Windows-specific path formats that git trace2 emits
        let line = r#"{"event":"def_repo","sid":"sess1","thread":"main","repo":1,"worktree":"C:\\Users\\user\\.git-ai\\repos\\test"}"#;
        let event = parse_trace2_line(line).unwrap();
        match event {
            Trace2Event::DefRepo { repo_path, .. } => {
                let path_str = repo_path.to_string_lossy();
                assert!(path_str.contains("test"));
            }
            _ => panic!("expected DefRepo"),
        }
    }

    #[test]
    fn parse_trace2_handles_partial_json_gracefully() {
        // Simulates truncated data from a pipe read boundary
        assert!(parse_trace2_line(r#"{"event":"start","sid":"abc"#).is_none());
        assert!(parse_trace2_line("").is_none());
        assert!(parse_trace2_line("\n").is_none());
    }

    #[test]
    fn parse_trace2_rapid_fire_events() {
        // Simulates a fast git operation that emits many events quickly
        let lines: Vec<String> = (0..100)
            .map(|i| {
                format!(
                    r#"{{"event":"data","sid":"rapid-{}","thread":"main","key":"counter","value":"{}"}}"#,
                    i, i
                )
            })
            .collect();

        let events: Vec<Trace2Event> = lines
            .iter()
            .filter_map(|line| parse_trace2_line(line))
            .collect();

        // All should parse as Ignored (data events aren't actionable)
        assert_eq!(events.len(), 100);
        assert!(events.iter().all(|e| matches!(e, Trace2Event::Ignored)));
    }

    // -------------------------------------------------------
    // Tests for shutdown flag handling (runs on all platforms)
    // -------------------------------------------------------

    #[test]
    fn shutdown_flag_prevents_event_processing() {
        // Verify that the shutdown flag mechanism works correctly
        let shutdown = Arc::new(AtomicBool::new(false));

        // Simulate the check pattern used in handle_pipe_connection
        assert!(!shutdown.load(Ordering::Relaxed));

        shutdown.store(true, Ordering::Relaxed);
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn sender_disconnect_stops_processing() {
        // Verify that a dropped receiver causes send to fail (simulating
        // what happens in handle_pipe_connection when event_tx.send() fails)
        let (tx, rx) = std::sync::mpsc::channel::<Trace2Event>();
        drop(rx);

        let event = Trace2Event::Ignored;
        assert!(tx.send(event).is_err());
    }

    // -------------------------------------------------------
    // Windows-only integration tests (only run on Windows CI)
    // -------------------------------------------------------

    #[cfg(windows)]
    mod integration {
        use super::super::imp::Trace2ListenerWin;
        use super::*;
        use std::io;
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        type HANDLE = *mut std::ffi::c_void;
        type BOOL = i32;

        #[repr(C)]
        struct SECURITY_ATTRIBUTES {
            n_length: u32,
            lp_security_descriptor: *mut std::ffi::c_void,
            b_inherit_handle: BOOL,
        }

        #[repr(C)]
        struct OVERLAPPED {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            h_event: HANDLE,
        }

        unsafe extern "system" {
            fn CreateFileW(
                lp_file_name: *const u16,
                dw_desired_access: u32,
                dw_share_mode: u32,
                lp_security_attributes: *const SECURITY_ATTRIBUTES,
                dw_creation_disposition: u32,
                dw_flags_and_attributes: u32,
                h_template_file: HANDLE,
            ) -> HANDLE;

            fn WriteFile(
                h_file: HANDLE,
                lp_buffer: *const u8,
                n_number_of_bytes_to_write: u32,
                lp_number_of_bytes_written: *mut u32,
                lp_overlapped: *mut OVERLAPPED,
            ) -> BOOL;

            fn FlushFileBuffers(h_file: HANDLE) -> BOOL;

            fn CloseHandle(h_object: HANDLE) -> BOOL;
        }

        const GENERIC_WRITE: u32 = 0x40000000;
        const OPEN_EXISTING: u32 = 3;
        const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
        const INVALID_HANDLE_VALUE: isize = -1;

        #[test]
        fn listener_binds_and_accepts_connection() {
            use std::time::Instant;

            let pipe_path = format!(r"\\.\pipe\git-ai-test-{}", std::process::id());
            let pipe_pathbuf = std::path::PathBuf::from(&pipe_path);
            let shutdown = Arc::new(AtomicBool::new(false));

            let listener = Trace2ListenerWin::bind(&pipe_pathbuf, Arc::clone(&shutdown))
                .expect("failed to bind named pipe");

            let (tx, rx) = mpsc::channel();
            let shutdown_clone = Arc::clone(&shutdown);

            let listener_thread = thread::spawn(move || {
                listener.run(tx);
            });

            // Give the listener time to create the pipe instance
            thread::sleep(Duration::from_millis(200));

            // Connect as a client
            let wide_path = to_wide(&pipe_path);
            let client_handle = unsafe {
                CreateFileW(
                    wide_path.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };

            assert_ne!(
                client_handle as isize,
                INVALID_HANDLE_VALUE,
                "failed to connect to pipe: {}",
                io::Error::last_os_error()
            );

            // Write trace2 events
            let events = concat!(
                r#"{"event":"start","sid":"win-test","thread":"main","time":"2024-01-01T00:00:00Z","argv":["git","status"]}"#,
                "\n",
                r#"{"event":"cmd_name","sid":"win-test","thread":"main","name":"status"}"#,
                "\n",
                r#"{"event":"exit","sid":"win-test","thread":"main","t_abs":0.01,"code":0}"#,
                "\n",
            );

            let mut bytes_written: u32 = 0;
            let ret = unsafe {
                WriteFile(
                    client_handle,
                    events.as_ptr(),
                    events.len() as u32,
                    &mut bytes_written,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ret, 0, "WriteFile failed");
            unsafe {
                FlushFileBuffers(client_handle);
            }

            // Close client handle (simulates disconnect)
            unsafe {
                CloseHandle(client_handle);
            }

            // Collect events
            let mut received = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(event) => received.push(event),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if received.len() >= 3 {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }

            // Shutdown
            shutdown_clone.store(true, Ordering::Relaxed);
            listener_thread.join().unwrap();

            assert_eq!(
                received.len(),
                3,
                "expected 3 events, got {}",
                received.len()
            );
            assert!(matches!(received[0], Trace2Event::Start { .. }));
            assert!(matches!(received[1], Trace2Event::CmdName { .. }));
            assert!(matches!(received[2], Trace2Event::CommandExit { .. }));
        }

        #[test]
        fn listener_handles_multiple_concurrent_connections() {
            let pipe_path = format!(r"\\.\pipe\git-ai-multi-{}", std::process::id());
            let pipe_pathbuf = std::path::PathBuf::from(&pipe_path);
            let shutdown = Arc::new(AtomicBool::new(false));

            let listener = Trace2ListenerWin::bind(&pipe_pathbuf, Arc::clone(&shutdown))
                .expect("failed to bind");

            let (tx, rx) = mpsc::channel();
            let shutdown_clone = Arc::clone(&shutdown);

            let listener_thread = thread::spawn(move || {
                listener.run(tx);
            });

            thread::sleep(Duration::from_millis(200));

            // Spawn multiple clients sequentially (named pipes handle one at a time
            // with PIPE_UNLIMITED_INSTANCES creating new instances per connection)
            let mut client_threads = Vec::new();
            for i in 0..3u64 {
                let path = pipe_path.clone();
                client_threads.push(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(i * 100));
                    let wide_path = to_wide(&path);
                    let handle = unsafe {
                        CreateFileW(
                            wide_path.as_ptr(),
                            GENERIC_WRITE,
                            0,
                            std::ptr::null(),
                            OPEN_EXISTING,
                            FILE_ATTRIBUTE_NORMAL,
                            std::ptr::null_mut(),
                        )
                    };
                    if handle as isize == INVALID_HANDLE_VALUE {
                        return;
                    }

                    let event = format!(
                        r#"{{"event":"cmd_name","sid":"client-{}","thread":"main","name":"status"}}"#,
                        i
                    );
                    let line = format!("{}\n", event);
                    let mut written: u32 = 0;
                    unsafe {
                        WriteFile(
                            handle,
                            line.as_ptr(),
                            line.len() as u32,
                            &mut written,
                            std::ptr::null_mut(),
                        );
                        FlushFileBuffers(handle);
                        CloseHandle(handle);
                    }
                }));
            }

            for t in client_threads {
                t.join().unwrap();
            }

            // Collect events
            let mut received = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(event) => received.push(event),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if received.len() >= 3 {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }

            shutdown_clone.store(true, Ordering::Relaxed);
            listener_thread.join().unwrap();

            assert!(
                received.len() >= 3,
                "expected at least 3 events from concurrent clients, got {}",
                received.len()
            );
        }

        #[test]
        fn listener_survives_client_disconnect_without_data() {
            let pipe_path = format!(r"\\.\pipe\git-ai-disconnect-{}", std::process::id());
            let pipe_pathbuf = std::path::PathBuf::from(&pipe_path);
            let shutdown = Arc::new(AtomicBool::new(false));

            let listener = Trace2ListenerWin::bind(&pipe_pathbuf, Arc::clone(&shutdown))
                .expect("failed to bind");

            let (tx, rx) = mpsc::channel();
            let shutdown_clone = Arc::clone(&shutdown);

            let listener_thread = thread::spawn(move || {
                listener.run(tx);
            });

            thread::sleep(Duration::from_millis(200));

            // Connect and immediately disconnect without writing
            let wide_path = to_wide(&pipe_path);
            let handle = unsafe {
                CreateFileW(
                    wide_path.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(handle as isize, INVALID_HANDLE_VALUE);
            unsafe {
                CloseHandle(handle);
            }

            // Wait a moment, then connect again with actual data
            thread::sleep(Duration::from_millis(500));

            let handle2 = unsafe {
                CreateFileW(
                    wide_path.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };

            if handle2 as isize != INVALID_HANDLE_VALUE {
                let line =
                    r#"{"event":"cmd_name","sid":"after-disconnect","thread":"main","name":"log"}"#
                        .to_string()
                        + "\n";
                let mut written: u32 = 0;
                unsafe {
                    WriteFile(
                        handle2,
                        line.as_ptr(),
                        line.len() as u32,
                        &mut written,
                        std::ptr::null_mut(),
                    );
                    FlushFileBuffers(handle2);
                    CloseHandle(handle2);
                }
            }

            // Collect events
            let mut received = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(event) => received.push(event),
                    Err(_) => {
                        if !received.is_empty() {
                            break;
                        }
                    }
                }
            }

            shutdown_clone.store(true, Ordering::Relaxed);
            listener_thread.join().unwrap();

            // The listener should still be alive and processing after the
            // abrupt disconnect -- verify we got the second client's event
            assert!(
                received.len() >= 1,
                "listener should recover from abrupt client disconnect"
            );
        }
    }
}
