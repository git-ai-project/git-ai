use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::config::{NotesBackendConfig, NotesBackendKind};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const OVERSIZED_RESPONSE_BYTES: usize = 64 * 1_024 * 1_024;

struct OversizedResponseServer {
    addr: SocketAddr,
    requested: Arc<AtomicBool>,
    response_finished: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl OversizedResponseServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let requested = Arc::new(AtomicBool::new(false));
        let response_finished = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let requested_for_thread = Arc::clone(&requested);
        let response_finished_for_thread = Arc::clone(&response_finished);
        let shutdown_for_thread = Arc::clone(&shutdown);
        let join = std::thread::spawn(move || {
            while !shutdown_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 16 * 1_024];
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.read(&mut request);
                        requested_for_thread.store(true, Ordering::Release);
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Content-Length: {OVERSIZED_RESPONSE_BYTES}\r\nConnection: close\r\n\r\n"
                        );
                        if stream.write_all(header.as_bytes()).is_ok() {
                            let chunk = [b'x'; 64 * 1_024];
                            for _ in 0..OVERSIZED_RESPONSE_BYTES / chunk.len() {
                                if stream.write_all(&chunk).is_err() {
                                    break;
                                }
                            }
                        }
                        response_finished_for_thread.store(true, Ordering::Release);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requested,
            response_finished,
            shutdown,
            join: Some(join),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn wait_for_response_completion(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.response_finished.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            self.requested.load(Ordering::Acquire),
            "daemon never requested the oversized HTTP response"
        );
        assert!(
            self.response_finished.load(Ordering::Acquire),
            "oversized HTTP response did not finish before the deadline"
        );
    }
}

impl Drop for OversizedResponseServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(target_os = "linux")]
fn daemon_hwm_kib(repo: &TestRepo) -> u64 {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmHWM:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
        .expect("daemon status should include VmHWM")
}

#[cfg(target_os = "linux")]
fn daemon_thread_count(repo: &TestRepo) -> u64 {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|line| {
            line.strip_prefix("Threads:")
                .and_then(|value| value.trim().parse().ok())
        })
        .expect("daemon status should include Threads")
}

#[cfg(target_os = "linux")]
fn wait_for_daemon_thread_bound(repo: &TestRepo, maximum: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current = daemon_thread_count(repo);
        if current <= maximum || Instant::now() >= deadline {
            return current;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn oversized_http_response_keeps_daemon_bounded_and_recovers() {
    let server = OversizedResponseServer::start();
    let backend_url = server.base_url();
    let mut repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_NOTES_BACKEND_KIND", "http"),
        ("GIT_AI_NOTES_BACKEND_URL", backend_url.as_str()),
        ("GIT_AI_API_KEY", "oversized-response-test-key"),
    ]);
    repo.patch_git_ai_config(|patch| {
        patch.notes_backend = Some(NotesBackendConfig {
            kind: NotesBackendKind::Http,
            backend_url: Some(backend_url.clone()),
        });
    });
    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.git_og(&["add", "base.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "base commit"]).unwrap();
    let mut base = repo.filename("base.txt");
    base.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    let base_branch = repo.current_branch();
    repo.git_og(&["switch", "-c", "http-source"]).unwrap();
    fs::write(repo.path().join("source.txt"), "source\n").unwrap();
    repo.git_og(&["add", "source.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "source commit"]).unwrap();
    let source_sha = repo.git_og(&["rev-parse", "HEAD"]).unwrap();
    base.assert_committed_lines(crate::lines!["base".unattributed_human()]);
    let mut source = repo.filename("source.txt");
    source.assert_committed_lines(crate::lines!["source".unattributed_human()]);
    repo.git_og(&["switch", &base_branch]).unwrap();
    base.assert_committed_lines(crate::lines!["base".unattributed_human()]);

    #[cfg(target_os = "linux")]
    let baseline_hwm = daemon_hwm_kib(&repo);
    #[cfg(target_os = "linux")]
    let baseline_threads = daemon_thread_count(&repo);

    repo.git_without_test_sync_for_test(&["cherry-pick", source_sha.trim()], &[])
        .unwrap();
    repo.sync_daemon();
    server.wait_for_response_completion();

    #[cfg(target_os = "linux")]
    {
        let hwm_growth_kib = daemon_hwm_kib(&repo).saturating_sub(baseline_hwm);
        eprintln!("oversized HTTP response HWM growth: {hwm_growth_kib} KiB");
        assert!(
            hwm_growth_kib < 40 * 1_024,
            "oversized HTTP response grew daemon HWM by {hwm_growth_kib} KiB"
        );
        let rejected_threads = wait_for_daemon_thread_bound(&repo, baseline_threads + 2);
        eprintln!(
            "oversized HTTP response threads: baseline={baseline_threads}, rejected={rejected_threads}"
        );
        assert!(
            rejected_threads <= baseline_threads + 2,
            "HTTP response rejection must not retain worker threads: baseline={baseline_threads}, rejected={rejected_threads}"
        );
    }
    base.assert_committed_lines(crate::lines!["base".unattributed_human()]);
    source.assert_committed_lines(crate::lines!["source".unattributed_human()]);

    fs::write(repo.path().join("recovery.txt"), "AI recovery\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "recovery.txt"])
        .unwrap();
    repo.git(&["add", "recovery.txt"]).unwrap();
    repo.git(&["commit", "-m", "AI commit after oversized HTTP response"])
        .unwrap();
    base.assert_committed_lines(crate::lines!["base".unattributed_human()]);
    source.assert_committed_lines(crate::lines!["source".unattributed_human()]);
    let mut recovery = repo.filename("recovery.txt");
    recovery.assert_committed_lines(crate::lines!["AI recovery".ai()]);
}
