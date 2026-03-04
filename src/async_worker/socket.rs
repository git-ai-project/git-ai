use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::utils::debug_log;

/// The socket filename within the .git/ai/ directory.
const SOCKET_FILENAME: &str = "async-worker.sock";

/// Compute the socket path for a given ai_dir (the .git/ai/ directory).
pub fn socket_path_for_ai_dir(ai_dir: &Path) -> PathBuf {
    ai_dir.join(SOCKET_FILENAME)
}

/// Write a length-prefixed message to a stream.
/// Format: [4-byte big-endian length][payload]
pub fn write_message(stream: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

/// Read a length-prefixed message from a stream.
/// Returns the payload bytes, or None if the connection was closed.
pub fn read_message(stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;

    // Sanity check: reject payloads larger than 64 MB
    if len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Message too large: {} bytes", len),
        ));
    }

    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(Some(buf))
}

// ── Unix socket implementation ──────────────────────────────────────────────

#[cfg(unix)]
pub mod platform {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    /// Try to connect to the worker socket and send a message.
    /// Returns Ok(true) if the message was sent successfully.
    /// Returns Ok(false) if the socket doesn't exist or isn't accepting connections.
    pub fn try_send_to_socket(socket_path: &Path, payload: &[u8]) -> io::Result<bool> {
        if !socket_path.exists() {
            return Ok(false);
        }
        match UnixStream::connect(socket_path) {
            Ok(mut stream) => {
                // Set a write timeout so we don't hang forever
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                write_message(&mut stream, payload)?;
                debug_log(&format!(
                    "Successfully sent {} bytes to async worker socket",
                    payload.len()
                ));
                Ok(true)
            }
            Err(e) => {
                debug_log(&format!("Failed to connect to async worker socket: {}", e));
                Ok(false)
            }
        }
    }

    /// Bind to the socket path, returning the listener.
    /// This is atomic: if another process already owns the socket, this fails.
    pub fn bind_socket(socket_path: &Path) -> io::Result<UnixListener> {
        // Remove stale socket file if it exists
        if socket_path.exists() {
            // Try connecting first to see if it's alive
            if UnixStream::connect(socket_path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "Socket is already owned by another worker",
                ));
            }
            // Stale socket - remove it
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        // Set non-blocking so we can implement timeout-based accept
        listener.set_nonblocking(true)?;
        Ok(listener)
    }

    /// Accept a connection on the listener with a timeout.
    /// Returns None if the timeout expires without a connection.
    pub fn accept_with_timeout(
        listener: &UnixListener,
        timeout: Duration,
    ) -> io::Result<Option<UnixStream>> {
        let start = std::time::Instant::now();
        loop {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    // Set the stream to blocking mode for reading
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                    return Ok(Some(stream));
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= timeout {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Clean up the socket file.
    pub fn cleanup_socket(socket_path: &Path) {
        let _ = std::fs::remove_file(socket_path);
    }
}

// ── Windows named pipe implementation ───────────────────────────────────────

#[cfg(windows)]
pub mod platform {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;

    /// Derive a named pipe path from the ai_dir.
    /// Named pipes on Windows use the \\.\pipe\ prefix.
    pub fn named_pipe_path(ai_dir: &Path) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(ai_dir.to_string_lossy().as_bytes());
        let hash = format!("{:x}", hasher.finalize());
        format!(r"\\.\pipe\git-ai-worker-{}", &hash[..16])
    }

    /// Try to connect to the worker named pipe and send a message.
    pub fn try_send_to_socket(socket_path: &Path, payload: &[u8]) -> io::Result<bool> {
        let pipe_name = named_pipe_path(socket_path);
        match OpenOptions::new().write(true).open(&pipe_name) {
            Ok(mut pipe) => {
                write_message(&mut pipe, payload)?;
                debug_log(&format!(
                    "Successfully sent {} bytes to async worker pipe",
                    payload.len()
                ));
                Ok(true)
            }
            Err(e) => {
                debug_log(&format!("Failed to connect to async worker pipe: {}", e));
                Ok(false)
            }
        }
    }

    /// Bind to a named pipe. On Windows we use a lock file for atomic ownership
    /// since named pipes don't have the same bind semantics as Unix sockets.
    pub fn bind_socket(socket_path: &Path) -> io::Result<()> {
        // On Windows, we rely on the LockFile mechanism for atomic ownership.
        // The actual pipe server is created in the worker loop.
        // This function just verifies no other worker owns the lock.
        let lock_path = socket_path.with_extension("lock");
        match crate::utils::LockFile::try_acquire(&lock_path) {
            Some(_lock) => {
                // We got the lock - but we drop it here; the worker will re-acquire.
                Ok(())
            }
            None => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Lock is already held by another worker",
            )),
        }
    }

    /// Clean up the socket/pipe files.
    pub fn cleanup_socket(socket_path: &Path) {
        let lock_path = socket_path.with_extension("lock");
        let _ = std::fs::remove_file(&lock_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_for_ai_dir() {
        let ai_dir = Path::new("/tmp/test-repo/.git/ai");
        let path = socket_path_for_ai_dir(ai_dir);
        assert_eq!(
            path,
            PathBuf::from("/tmp/test-repo/.git/ai/async-worker.sock")
        );
    }

    #[test]
    fn test_write_read_message_roundtrip() {
        let payload = b"hello world";
        let mut buf = Vec::new();
        write_message(&mut buf, payload).unwrap();

        assert_eq!(buf.len(), 4 + payload.len());

        let mut cursor = std::io::Cursor::new(buf);
        let result = read_message(&mut cursor).unwrap();
        assert_eq!(result, Some(payload.to_vec()));
    }

    #[test]
    fn test_read_message_empty_stream() {
        let buf: Vec<u8> = vec![];
        let mut cursor = std::io::Cursor::new(buf);
        let result = read_message(&mut cursor).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_read_message_rejects_oversized() {
        // Create a message header claiming 128MB
        let len: u32 = 128 * 1024 * 1024;
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&[0u8; 16]); // some dummy data

        let mut cursor = std::io::Cursor::new(buf);
        let result = read_message(&mut cursor);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_unix_socket_bind_and_connect() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();
        assert!(socket_path.exists());

        // Should be able to connect
        let payload = b"test message";
        let sent = platform::try_send_to_socket(&socket_path, payload).unwrap();
        assert!(sent);

        // Accept the connection and read the message (use longer timeout for CI)
        let accept_result =
            platform::accept_with_timeout(&listener, Duration::from_secs(5)).unwrap();
        assert!(accept_result.is_some(), "Should accept connection");
        let mut stream = accept_result.unwrap();

        let msg = read_message(&mut stream).unwrap().unwrap();
        assert_eq!(msg, payload);

        platform::cleanup_socket(&socket_path);
        assert!(!socket_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_unix_socket_nonexistent_returns_false() {
        let path = Path::new("/tmp/nonexistent-git-ai-test.sock");
        let result = platform::try_send_to_socket(path, b"hello").unwrap();
        assert!(!result);
    }
}
