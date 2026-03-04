use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::utils::debug_log;
use interprocess::local_socket::{
    GenericFilePath, ListenerNonblockingMode, ListenerOptions, ToFsName,
};

/// The socket filename within the .git/ai/ directory.
const SOCKET_FILENAME: &str = "async-worker.sock";

/// Compute the socket path for a given ai_dir (the .git/ai/ directory).
pub fn socket_path_for_ai_dir(ai_dir: &Path) -> PathBuf {
    ai_dir.join(SOCKET_FILENAME)
}

/// Write a length-prefixed message to a stream.
///
/// Wire format: 4-byte big-endian length followed by the payload bytes.
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

// ── Cross-platform socket implementation using interprocess ─────────────────

/// On Windows, interprocess uses named pipes, which require a `\\.\pipe\` path.
/// We hash the socket_path to derive a stable pipe name.
#[cfg(windows)]
fn to_local_socket_name(
    socket_path: &Path,
) -> io::Result<interprocess::local_socket::Name<'static>> {
    use interprocess::local_socket::ToNsName;
    // Use a hash of the path to create a unique namespaced name
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(socket_path.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let name = format!("git-ai-worker-{}", &hash[..16]);
    name.to_ns_name::<interprocess::local_socket::GenericNamespaced>()
}

/// On Unix, use the filesystem path directly as a Unix domain socket.
#[cfg(unix)]
fn to_local_socket_name(socket_path: &Path) -> io::Result<interprocess::local_socket::Name<'_>> {
    socket_path.to_fs_name::<GenericFilePath>()
}

pub mod platform {
    use super::*;
    use interprocess::local_socket::prelude::*;

    /// Try to connect to the worker socket and send a message.
    /// Returns Ok(true) if the message was sent successfully.
    /// Returns Ok(false) if the socket doesn't exist or isn't accepting connections.
    pub fn try_send_to_socket(socket_path: &Path, payload: &[u8]) -> io::Result<bool> {
        // On Unix, check file existence first (fast path).
        #[cfg(unix)]
        if !socket_path.exists() {
            return Ok(false);
        }

        let name = match to_local_socket_name(socket_path) {
            Ok(name) => name,
            Err(_) => return Ok(false),
        };

        match LocalSocketStream::connect(name) {
            Ok(mut stream) => {
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

    /// Probe whether a worker is listening on the socket.
    /// Returns true if a connection can be established (without sending data).
    pub fn is_socket_live(socket_path: &Path) -> bool {
        #[cfg(unix)]
        if !socket_path.exists() {
            return false;
        }

        let name = match to_local_socket_name(socket_path) {
            Ok(name) => name,
            Err(_) => return false,
        };
        LocalSocketStream::connect(name).is_ok()
    }

    /// Bind to the socket path, returning the listener.
    /// This is atomic: if another process already owns the socket, this fails.
    pub fn bind_socket(socket_path: &Path) -> io::Result<interprocess::local_socket::Listener> {
        // On Unix, check if there's already a live listener
        #[cfg(unix)]
        if socket_path.exists() {
            if is_socket_live(socket_path) {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "Socket is already owned by another worker",
                ));
            }
            // Stale socket file - remove it so we can rebind
            let _ = std::fs::remove_file(socket_path);
        }

        let name = to_local_socket_name(socket_path)?;

        ListenerOptions::new().name(name).create_sync()
    }

    /// Accept a connection on the listener with a timeout.
    /// Returns None if the timeout expires without a connection.
    pub fn accept_with_timeout(
        listener: &interprocess::local_socket::Listener,
        timeout: Duration,
    ) -> io::Result<Option<interprocess::local_socket::Stream>> {
        // Set the listener to non-blocking accept mode
        listener.set_nonblocking(ListenerNonblockingMode::Accept)?;

        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(50);

        loop {
            match listener.accept() {
                Ok(stream) => {
                    // Restore blocking mode
                    listener.set_nonblocking(ListenerNonblockingMode::Neither)?;
                    return Ok(Some(stream));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= timeout {
                        listener.set_nonblocking(ListenerNonblockingMode::Neither)?;
                        return Ok(None);
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(e) => {
                    let _ = listener.set_nonblocking(ListenerNonblockingMode::Neither);
                    return Err(e);
                }
            }
        }
    }

    /// Drain any pending connections from the listener backlog.
    /// Processes each pending job before returning.
    /// This prevents losing jobs during worker shutdown.
    pub fn drain_pending(
        listener: &interprocess::local_socket::Listener,
        mut process_fn: impl FnMut(&mut interprocess::local_socket::Stream),
    ) {
        // Set non-blocking to drain without waiting
        if listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .is_err()
        {
            return;
        }

        while let Ok(mut stream) = listener.accept() {
            process_fn(&mut stream);
        }

        let _ = listener.set_nonblocking(ListenerNonblockingMode::Neither);
    }

    /// Clean up the socket file.
    pub fn cleanup_socket(socket_path: &Path) {
        let _ = std::fs::remove_file(socket_path);
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

    #[test]
    fn test_socket_bind_and_connect() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let listener = platform::bind_socket(&socket_path).unwrap();

        // Send the message from a background thread
        let sender_path = socket_path.clone();
        let sender = std::thread::spawn(move || {
            // Small delay to ensure the accept loop is running
            std::thread::sleep(Duration::from_millis(100));
            let sent = platform::try_send_to_socket(&sender_path, b"test message").unwrap();
            assert!(sent);
        });

        // Accept the connection and read the message
        let accept_result =
            platform::accept_with_timeout(&listener, Duration::from_secs(5)).unwrap();
        assert!(accept_result.is_some(), "Should accept connection");
        let mut stream = accept_result.unwrap();

        let msg = read_message(&mut stream).unwrap().unwrap();
        assert_eq!(msg, b"test message");

        sender.join().unwrap();

        platform::cleanup_socket(&socket_path);
    }

    #[test]
    fn test_socket_nonexistent_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent-git-ai-test.sock");
        let result = platform::try_send_to_socket(&path, b"hello").unwrap();
        assert!(!result);
    }
}
