use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

pub struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

const FRAMES: &[&str] = &[
    "\u{2807}", "\u{280b}", "\u{2819}", "\u{2838}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}",
    "\u{280f}", "\u{2839}",
];

impl Spinner {
    pub fn new(message: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        let msg = message.to_string();
        let handle = thread::spawn(move || {
            let mut i = 0;
            while r.load(Ordering::Relaxed) {
                let frame = FRAMES[i % FRAMES.len()];
                eprint!("\r\x1b[32m{}\x1b[0m {}", frame, msg);
                let _ = std::io::stderr().flush();
                i += 1;
                thread::sleep(Duration::from_millis(100));
            }
        });
        Self {
            running,
            handle: Some(handle),
        }
    }

    pub fn start(&self) {}

    #[allow(dead_code)]
    pub fn update_message(&self, _message: &str) {}

    #[allow(dead_code)]
    pub async fn wait_for(&self, duration_ms: u64) {
        tokio::time::sleep(Duration::from_millis(duration_ms)).await;
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }

    pub fn success(&self, message: &str) {
        self.finish();
        println!("\x1b[1;32m\u{2713} {}\x1b[0m", message);
    }

    pub fn pending(&self, message: &str) {
        self.finish();
        println!("\x1b[1;33m\u{26a0} {}\x1b[0m", message);
    }

    pub fn error(&self, message: &str) {
        self.finish();
        println!("\x1b[1;31m\u{2717} {}\x1b[0m", message);
    }

    #[allow(dead_code)]
    pub fn skipped(&self, message: &str) {
        self.finish();
        println!("\x1b[90m\u{25cb} {}\x1b[0m", message);
    }

    fn finish(&self) {
        self.running.store(false, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(15));
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn print_diff(diff_text: &str) {
    for line in diff_text.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            println!("\x1b[1m{}\x1b[0m", line);
        } else if line.starts_with('+') {
            println!("\x1b[32m{}\x1b[0m", line);
        } else if line.starts_with('-') {
            println!("\x1b[31m{}\x1b[0m", line);
        } else if line.starts_with("@@") {
            println!("\x1b[36m{}\x1b[0m", line);
        } else {
            println!("{}", line);
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_creation() {
        let spinner = Spinner::new("Testing spinner");
        spinner.start();
    }

    #[test]
    fn test_spinner_success_output() {
        let spinner = Spinner::new("Processing");
        spinner.success("Operation completed successfully");
    }

    #[test]
    fn test_spinner_pending_output() {
        let spinner = Spinner::new("Processing");
        spinner.pending("Pending action required");
    }

    #[test]
    fn test_spinner_error_output() {
        let spinner = Spinner::new("Processing");
        spinner.error("An error occurred");
    }

    #[test]
    fn test_spinner_skipped_output() {
        let spinner = Spinner::new("Processing");
        spinner.skipped("Operation skipped");
    }

    #[test]
    fn test_spinner_update_message() {
        let spinner = Spinner::new("Initial message");
        spinner.update_message("Updated message");
        spinner.success("Done");
    }

    #[test]
    fn test_print_diff_additions() {
        let diff = "+new line\n+another new line";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_deletions() {
        let diff = "-removed line\n-another removed line";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_file_headers() {
        let diff = "--- a/file.txt\n+++ b/file.txt";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_hunk_headers() {
        let diff = "@@ -1,3 +1,4 @@";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_context_lines() {
        let diff = " context line 1\n context line 2";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_complete() {
        let diff = "--- a/test.txt\n+++ b/test.txt\n@@ -1,3 +1,4 @@\n context\n-old line\n+new line\n context";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_empty() {
        let diff = "";
        print_diff(diff);
    }

    #[test]
    fn test_print_diff_multiline() {
        let diff = "--- a/file.rs\n+++ b/file.rs\n@@ -10,5 +10,6 @@\n fn main() {\n-    println!(\"old\");\n+    println!(\"new\");\n+    println!(\"extra\");\n }";
        print_diff(diff);
    }
}
