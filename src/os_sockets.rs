//! Native AF_UNIX socket implementation for both Windows and Unix platforms.
//!
//! This module provides a unified interface for Unix domain sockets across platforms:
//! - On Unix: uses std::os::unix::net directly
//! - On Windows: uses native Winsock2 AF_UNIX APIs via windows-sys

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};

    pub struct UnixStream(StdUnixStream);

    impl UnixStream {
        pub fn connect(path: &Path) -> io::Result<Self> {
            StdUnixStream::connect(path).map(UnixStream)
        }

        pub fn connect_timeout(path: &Path, timeout: Duration) -> io::Result<Self> {
            // Unix doesn't have a built-in connect_timeout for Unix sockets.
            // Spawn blocking connect in a thread and join with timeout.
            use std::sync::mpsc;
            use std::thread;

            let path = path.to_path_buf();
            let (tx, rx) = mpsc::channel();

            thread::spawn(move || {
                let result = StdUnixStream::connect(&path);
                let _ = tx.send(result);
            });

            match rx.recv_timeout(timeout) {
                Ok(Ok(sock)) => {
                    sock.set_read_timeout(Some(timeout))?;
                    sock.set_write_timeout(Some(timeout))?;
                    Ok(UnixStream(sock))
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("connection timed out after {:?}", timeout),
                )),
            }
        }

        pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }

        pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(timeout)
        }
    }

    impl Read for UnixStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for UnixStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    pub struct UnixListener(StdUnixListener);

    impl UnixListener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            StdUnixListener::bind(path).map(UnixListener)
        }

        pub fn accept(&self) -> io::Result<UnixStream> {
            self.0.accept().map(|(stream, _)| UnixStream(stream))
        }

        pub fn incoming(&self) -> Incoming<'_> {
            Incoming { listener: self }
        }
    }

    pub struct Incoming<'a> {
        listener: &'a UnixListener,
    }

    impl<'a> Iterator for Incoming<'a> {
        type Item = io::Result<UnixStream>;

        fn next(&mut self) -> Option<Self::Item> {
            Some(self.listener.accept())
        }
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::mem;
    use std::os::raw::c_int;
    use std::ptr;
    use std::sync::Once;
    use windows_sys::Win32::Foundation::{HANDLE, SetHandleInformation};
    use windows_sys::Win32::Networking::WinSock::{
        AF_UNIX, INVALID_SOCKET, SO_RCVTIMEO, SO_SNDTIMEO, SOCK_STREAM, SOCKADDR, SOCKADDR_UN,
        SOCKET, SOL_SOCKET, SOMAXCONN, WSA_FLAG_OVERLAPPED, WSADATA, WSAGetLastError, WSASocketW,
        WSAStartup, accept, bind, closesocket, connect, listen, recv, send,
        setsockopt as c_setsockopt,
    };

    const HANDLE_FLAG_INHERIT: u32 = 0x01;
    const UNIX_PATH_MAX: usize = 108;

    /// Initialize Windows Sockets API
    fn init_winsock() {
        static INIT: Once = Once::new();
        INIT.call_once(|| unsafe {
            let mut data: WSADATA = mem::zeroed();
            let ret = WSAStartup(0x202, &mut data); // version 2.2
            assert_eq!(ret, 0, "WSAStartup failed");
        });
    }

    fn last_error() -> io::Error {
        io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
    }

    fn cvt(result: c_int) -> io::Result<c_int> {
        if result == -1 {
            Err(last_error())
        } else {
            Ok(result)
        }
    }

    /// Convert a path to a SOCKADDR_UN structure
    fn path_to_sockaddr(path: &Path) -> io::Result<(SOCKADDR_UN, c_int)> {
        let path_str = path.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path must be valid UTF-8")
        })?;

        // Windows AF_UNIX uses UTF-8 encoded paths
        let path_bytes = path_str.as_bytes();
        if path_bytes.len() >= UNIX_PATH_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path too long (max {} bytes)", UNIX_PATH_MAX - 1),
            ));
        }

        let mut addr: SOCKADDR_UN = unsafe { mem::zeroed() };
        addr.sun_family = AF_UNIX;

        // Copy path bytes into sun_path
        unsafe {
            ptr::copy_nonoverlapping(
                path_bytes.as_ptr(),
                addr.sun_path.as_mut_ptr() as *mut u8,
                path_bytes.len(),
            );
        }

        let addr_len = mem::size_of_val(&addr.sun_family) + path_bytes.len() + 1;
        Ok((addr, addr_len as c_int))
    }

    pub struct UnixStream {
        socket: SOCKET,
    }

    impl UnixStream {
        pub fn connect(path: &Path) -> io::Result<Self> {
            init_winsock();

            let socket = unsafe {
                WSASocketW(
                    AF_UNIX as i32,
                    SOCK_STREAM,
                    0,
                    ptr::null_mut(),
                    0,
                    WSA_FLAG_OVERLAPPED,
                )
            };

            if socket == INVALID_SOCKET {
                return Err(last_error());
            }

            // Disable handle inheritance
            unsafe {
                SetHandleInformation(socket as HANDLE, HANDLE_FLAG_INHERIT, 0);
            }

            let (addr, addr_len) = match path_to_sockaddr(path) {
                Ok(v) => v,
                Err(e) => {
                    unsafe { closesocket(socket) };
                    return Err(e);
                }
            };
            let result = unsafe { connect(socket, &addr as *const _ as *const SOCKADDR, addr_len) };

            if result == -1 {
                unsafe { closesocket(socket) };
                return Err(last_error());
            }

            Ok(UnixStream { socket })
        }

        pub fn connect_timeout(path: &Path, timeout: Duration) -> io::Result<Self> {
            // Spawn blocking connect in a thread and join with timeout.
            use std::sync::mpsc;
            use std::thread;

            let path = path.to_path_buf();
            let (tx, rx) = mpsc::channel();

            thread::spawn(move || {
                let result = Self::connect(&path);
                let _ = tx.send(result);
            });

            match rx.recv_timeout(timeout) {
                Ok(Ok(stream)) => {
                    stream.set_read_timeout(Some(timeout))?;
                    stream.set_write_timeout(Some(timeout))?;
                    Ok(stream)
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("connection timed out after {:?}", timeout),
                )),
            }
        }

        pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.set_timeout(timeout, SO_RCVTIMEO)
        }

        pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.set_timeout(timeout, SO_SNDTIMEO)
        }

        fn set_timeout(&self, dur: Option<Duration>, kind: c_int) -> io::Result<()> {
            let timeout_ms = match dur {
                Some(d) => d.as_millis().min(u32::MAX as u128) as u32,
                None => 0,
            };

            unsafe {
                cvt(c_setsockopt(
                    self.socket,
                    SOL_SOCKET,
                    kind,
                    &timeout_ms as *const _ as *const u8,
                    mem::size_of::<u32>() as c_int,
                ))?;
            }
            Ok(())
        }
    }

    impl Read for UnixStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let ret = unsafe {
                recv(
                    self.socket,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as c_int,
                    0,
                )
            };
            if ret == -1 {
                Err(last_error())
            } else {
                Ok(ret as usize)
            }
        }
    }

    impl Write for UnixStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let ret = unsafe { send(self.socket, buf.as_ptr() as *const _, buf.len() as c_int, 0) };
            if ret == -1 {
                Err(last_error())
            } else {
                Ok(ret as usize)
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for UnixStream {
        fn drop(&mut self) {
            unsafe {
                closesocket(self.socket);
            }
        }
    }

    pub struct UnixListener {
        socket: SOCKET,
    }

    impl UnixListener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            init_winsock();

            // Remove existing socket file if it exists
            let _ = std::fs::remove_file(path);

            let socket = unsafe {
                WSASocketW(
                    AF_UNIX as i32,
                    SOCK_STREAM,
                    0,
                    ptr::null_mut(),
                    0,
                    WSA_FLAG_OVERLAPPED,
                )
            };

            if socket == INVALID_SOCKET {
                return Err(last_error());
            }

            // Disable handle inheritance
            unsafe {
                SetHandleInformation(socket as HANDLE, HANDLE_FLAG_INHERIT, 0);
            }

            let (addr, addr_len) = match path_to_sockaddr(path) {
                Ok(v) => v,
                Err(e) => {
                    unsafe { closesocket(socket) };
                    return Err(e);
                }
            };
            let bind_result =
                unsafe { bind(socket, &addr as *const _ as *const SOCKADDR, addr_len) };

            if bind_result == -1 {
                unsafe { closesocket(socket) };
                return Err(last_error());
            }

            let listen_result = unsafe { listen(socket, SOMAXCONN as i32) };
            if listen_result == -1 {
                unsafe { closesocket(socket) };
                return Err(last_error());
            }

            Ok(UnixListener { socket })
        }

        pub fn accept(&self) -> io::Result<UnixStream> {
            let socket = unsafe { accept(self.socket, ptr::null_mut(), ptr::null_mut()) };

            if socket == INVALID_SOCKET {
                return Err(last_error());
            }

            // Disable handle inheritance for accepted socket
            unsafe {
                SetHandleInformation(socket as HANDLE, HANDLE_FLAG_INHERIT, 0);
            }

            Ok(UnixStream { socket })
        }

        pub fn incoming(&self) -> Incoming<'_> {
            Incoming { listener: self }
        }
    }

    impl Drop for UnixListener {
        fn drop(&mut self) {
            unsafe {
                closesocket(self.socket);
            }
        }
    }

    pub struct Incoming<'a> {
        listener: &'a UnixListener,
    }

    impl<'a> Iterator for Incoming<'a> {
        type Item = io::Result<UnixStream>;

        fn next(&mut self) -> Option<Self::Item> {
            Some(self.listener.accept())
        }
    }
}

// Re-export the platform-specific implementations
#[cfg(unix)]
pub use unix_impl::{Incoming, UnixListener, UnixStream};

#[cfg(windows)]
pub use windows_impl::{Incoming, UnixListener, UnixStream};
