//! In-memory reference implementation of the notes backend HTTP server.
//!
//! This module exists to make the wire contract between `git-ai` and a
//! third-party notes backend self-documenting and locally runnable. It is NOT
//! intended for production use:
//!
//!   - All notes live in an `Arc<Mutex<HashMap>>` for the lifetime of the
//!     process.
//!   - Authentication is accepted but not validated.
//!   - Concurrency is "thread-per-connection" with no rate limiting.
//!
//! What it IS good for:
//!
//!   - Demonstrating exactly which endpoints a real backend must implement,
//!     what the request and response bodies look like, and which status codes
//!     `git-ai` distinguishes between.
//!   - Driving local end-to-end tests and benchmarks without a real server.
//!   - Serving as a starting point for a real implementation.
//!
//! # Wire contract
//!
//! The client side lives in `src/api/notes.rs`. The two endpoints are:
//!
//! ## `POST /worker/notes/upload`
//!
//! Request body (JSON, [`NotesUploadRequest`]):
//!
//! ```json
//! {
//!   "entries": [
//!     { "commit_sha": "<hex>", "content": "<authorship-log-string>" },
//!     ...
//!   ]
//! }
//! ```
//!
//! Response body (JSON, [`NotesUploadResponse`]):
//!
//! ```json
//! { "success_count": <int>, "failure_count": <int> }
//! ```
//!
//! Status: `200` on success; `400` on a malformed body.
//!
//! ## `GET /worker/notes/?commits=<sha1>,<sha2>,...`
//!
//! Response body (JSON, [`NotesReadResponse`]):
//!
//! ```json
//! { "notes": { "<sha>": "<content>", ... } }
//! ```
//!
//! Status: `200` if at least one of the requested SHAs is known; `404`
//! otherwise (the client treats `404` as "no notes found" — equivalent to an
//! empty map).
//!
//! [`NotesUploadRequest`]: crate::api::types::NotesUploadRequest
//! [`NotesUploadResponse`]: crate::api::types::NotesUploadResponse
//! [`NotesReadResponse`]: crate::api::types::NotesReadResponse

use crate::api::types::{NotesReadResponse, NotesUploadRequest, NotesUploadResponse};
use crate::error::GitAiError;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// In-memory, thread-safe note store.
#[derive(Default, Clone)]
pub struct NotesStore {
    inner: Arc<Mutex<HashMap<String, String>>>,
}

impl NotesStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / overwrite a single note. Returns `true` if a previous value
    /// was overwritten.
    pub fn put(&self, commit_sha: String, content: String) -> bool {
        let mut guard = self.inner.lock().expect("notes store poisoned");
        guard.insert(commit_sha, content).is_some()
    }

    /// Look up a single commit SHA.
    pub fn get(&self, commit_sha: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("notes store poisoned")
            .get(commit_sha)
            .cloned()
    }

    /// Look up many commit SHAs at once. Missing SHAs are absent from the map.
    pub fn get_many(&self, commit_shas: &[&str]) -> HashMap<String, String> {
        let guard = self.inner.lock().expect("notes store poisoned");
        commit_shas
            .iter()
            .filter_map(|sha| guard.get(*sha).map(|c| (sha.to_string(), c.clone())))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("notes store poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Handle to a running reference server. The server runs on a background
/// thread; dropping the handle (or calling [`ReferenceServer::shutdown`])
/// stops the accept loop.
pub struct ReferenceServer {
    addr: SocketAddr,
    store: NotesStore,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ReferenceServer {
    /// Bind to `bind_addr` (e.g. `127.0.0.1:0`) and spawn the accept loop on a
    /// background thread. The returned handle exposes the bound address (so
    /// callers binding to port `0` can discover the chosen port) and the
    /// shared [`NotesStore`].
    pub fn start(bind_addr: &str) -> Result<Self, GitAiError> {
        let listener = TcpListener::bind(bind_addr)
            .map_err(|e| GitAiError::Generic(format!("bind {}: {}", bind_addr, e)))?;
        // Short read timeout so the accept loop can periodically observe the
        // shutdown flag without needing a separate wakeup mechanism.
        listener
            .set_nonblocking(false)
            .map_err(GitAiError::IoError)?;

        let addr = listener.local_addr().map_err(GitAiError::IoError)?;
        let store = NotesStore::new();
        let shutdown = Arc::new(AtomicBool::new(false));

        let store_clone = store.clone();
        let shutdown_clone = shutdown.clone();
        let join = std::thread::Builder::new()
            .name("notes-reference-server".into())
            .spawn(move || accept_loop(listener, store_clone, shutdown_clone))
            .map_err(GitAiError::IoError)?;

        Ok(Self {
            addr,
            store,
            shutdown,
            join: Some(join),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn store(&self) -> &NotesStore {
        &self.store
    }

    /// Signal the accept loop to stop and wait for the thread to exit.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the accept loop by connecting to ourselves.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ReferenceServer {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

/// Run the server on the current thread until `Ctrl-C`. Used by the
/// `git-ai notes serve` CLI entry point.
pub fn run_blocking(bind_addr: &str) -> Result<(), GitAiError> {
    let server = ReferenceServer::start(bind_addr)?;
    eprintln!(
        "notes reference server listening on http://{}\n\
         (in-memory; not for production)\n\
         press Ctrl-C to stop.",
        server.addr()
    );
    // Block until the spawned thread exits — which only happens via shutdown,
    // which the CLI never triggers, so this is effectively `park forever`.
    if let Some(handle) = server.join.as_ref() {
        // Park the main thread; the OS will deliver SIGINT to the process and
        // tear everything down. We park in a loop because spurious wakeups
        // would otherwise cause this to return early.
        while !handle.is_finished() {
            std::thread::park();
        }
    }
    Ok(())
}

fn accept_loop(listener: TcpListener, store: NotesStore, shutdown: Arc<AtomicBool>) {
    for stream in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(stream) => {
                let store = store.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, &store) {
                        eprintln!("notes-reference-server: connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                eprintln!("notes-reference-server: accept error: {}", e);
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Minimal HTTP/1.1 request handling
// ----------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
}

fn handle_connection(mut stream: TcpStream, store: &NotesStore) -> Result<(), GitAiError> {
    let request = read_request(&mut stream)?;
    let response = dispatch(&request, store);
    write_response(&mut stream, &response)
}

fn read_request(stream: &mut TcpStream) -> Result<Request, GitAiError> {
    let mut reader = BufReader::new(stream.try_clone().map_err(GitAiError::IoError)?);

    // Request line.
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(GitAiError::IoError)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();

    // Split target into path + query.
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    // Headers (read until empty line, only `Content-Length` matters here).
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(GitAiError::IoError)?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    // Body — cap at 50 MB to prevent OOM from malformed Content-Length.
    const MAX_BODY: usize = 50 * 1024 * 1024;
    if content_length > MAX_BODY {
        return Err(GitAiError::Generic(format!(
            "Content-Length {} exceeds maximum {}",
            content_length, MAX_BODY
        )));
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).map_err(GitAiError::IoError)?;
    }

    Ok(Request {
        method,
        path,
        query,
        body,
    })
}

struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Response {
    fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            body: serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self::json(status, &serde_json::json!({ "error": message }))
    }
}

fn dispatch(req: &Request, store: &NotesStore) -> Response {
    // Tolerate the trailing slash variant the client sends (`/worker/notes/`)
    // as well as the bare path.
    let path = req.path.trim_end_matches('/');
    match (req.method.as_str(), path) {
        ("POST", "/worker/notes/upload") => handle_upload(&req.body, store),
        ("GET", "/worker/notes") => handle_read(&req.query, store),
        _ => Response::error(404, "not found"),
    }
}

fn handle_upload(body: &[u8], store: &NotesStore) -> Response {
    let request: NotesUploadRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return Response::error(400, &format!("invalid request body: {}", e)),
    };

    let mut success_count = 0usize;
    let failure_count = 0usize;
    for entry in request.entries {
        store.put(entry.commit_sha, entry.content);
        success_count += 1;
    }

    let response = NotesUploadResponse {
        success_count,
        failure_count,
    };
    Response::json(
        200,
        &serde_json::to_value(response).expect("serialise upload response"),
    )
}

fn handle_read(query: &str, store: &NotesStore) -> Response {
    // The client sends `commits=sha1,sha2,...`. We accept either form.
    let commits: Vec<&str> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| *k == "commits")
        .flat_map(|(_, v)| v.split(','))
        .filter(|s| !s.is_empty())
        .collect();

    let notes = store.get_many(&commits);

    if notes.is_empty() {
        // The client treats `404` as success-with-empty; we mirror that here
        // (rather than returning `200` + empty map) to exercise the cold-miss
        // path of the wire contract.
        return Response::json(404, &serde_json::json!({ "notes": {} }));
    }

    let response = NotesReadResponse { notes };
    Response::json(
        200,
        &serde_json::to_value(response).expect("serialise read response"),
    )
}

fn write_response(stream: &mut TcpStream, response: &Response) -> Result<(), GitAiError> {
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        response.status,
        reason,
        response.body.len()
    );
    stream
        .write_all(header.as_bytes())
        .map_err(GitAiError::IoError)?;
    stream
        .write_all(&response.body)
        .map_err(GitAiError::IoError)?;
    stream.flush().map_err(GitAiError::IoError)?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::{ApiClient, ApiContext};
    use crate::api::types::{NoteEntry, NotesUploadRequest};

    fn client_for(server: &ReferenceServer) -> ApiClient {
        // `ApiContext::without_auth` still picks up an API key from the
        // environment if one is set. The reference server ignores headers, so
        // either way is fine.
        ApiClient::new(ApiContext::without_auth(Some(server.base_url())))
    }

    #[test]
    fn upload_then_read_round_trip() {
        let server = ReferenceServer::start("127.0.0.1:0").expect("start server");
        let client = client_for(&server);

        let entries = vec![
            NoteEntry {
                commit_sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                content: "note-a".to_string(),
            },
            NoteEntry {
                commit_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                content: "note-b".to_string(),
            },
        ];
        let upload = client
            .upload_notes(NotesUploadRequest {
                entries: entries.clone(),
            })
            .expect("upload");
        assert_eq!(upload.success_count, 2);
        assert_eq!(upload.failure_count, 0);

        let shas: Vec<&str> = entries.iter().map(|e| e.commit_sha.as_str()).collect();
        let read = client.read_notes(&shas).expect("read");
        assert_eq!(read.notes.len(), 2);
        assert_eq!(
            read.notes.get(&entries[0].commit_sha).map(|s| s.as_str()),
            Some("note-a")
        );
        assert_eq!(
            read.notes.get(&entries[1].commit_sha).map(|s| s.as_str()),
            Some("note-b")
        );
    }

    #[test]
    fn read_unknown_sha_returns_empty() {
        let server = ReferenceServer::start("127.0.0.1:0").expect("start server");
        let client = client_for(&server);

        let read = client
            .read_notes(&["0000000000000000000000000000000000000000"])
            .expect("read");
        assert!(read.notes.is_empty());
    }

    #[test]
    fn upload_rejects_malformed_body() {
        let server = ReferenceServer::start("127.0.0.1:0").expect("start server");

        // Bypass `ApiClient` so we can send invalid JSON directly.
        let mut stream = TcpStream::connect(server.addr()).expect("connect");
        let body = b"not json";
        let request = format!(
            "POST /worker/notes/upload HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("write head");
        stream.write_all(body).expect("write body");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        assert!(response.starts_with("HTTP/1.1 400"), "got: {}", response);
    }

    #[test]
    fn store_is_shared_across_connections() {
        let server = ReferenceServer::start("127.0.0.1:0").expect("start server");
        let store = server.store().clone();

        let client = client_for(&server);
        client
            .upload_notes(NotesUploadRequest {
                entries: vec![NoteEntry {
                    commit_sha: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
                    content: "x".to_string(),
                }],
            })
            .expect("upload");

        // The store handed back by `server.store()` reflects writes that came
        // in over the socket — proving the in-memory store really is shared.
        assert_eq!(
            store.get("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            Some("x".to_string())
        );
    }
}
