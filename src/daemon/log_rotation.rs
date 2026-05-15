//! Log rotation for the daemon.
//!
//! On daemon start, checks if the log file exceeds 10MB and rotates:
//! - daemon.log -> daemon.log.1
//! - daemon.log.1 -> daemon.log.2
//! - daemon.log.2 -> daemon.log.3
//! - Anything older than .3 is deleted
//!
//! This is called synchronously before the daemon opens its log file.

use std::fs;
use std::path::Path;

pub const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024; // 10MB
const MAX_ROTATED_LOGS: u32 = 3;

/// Rotate log files if the current log exceeds 10MB.
///
/// Rotation scheme:
/// - `daemon.log` -> `daemon.log.1`
/// - `daemon.log.1` -> `daemon.log.2`
/// - `daemon.log.2` -> `daemon.log.3`
/// - Files older than `.3` are deleted.
///
/// This must be called before the daemon opens its log file for writing.
pub fn rotate_logs_if_needed(log_path: &Path) {
    let size = match fs::metadata(log_path) {
        Ok(meta) => meta.len(),
        Err(_) => return, // File doesn't exist or can't be read; nothing to rotate
    };

    if size <= MAX_LOG_SIZE {
        return;
    }

    // Delete the oldest rotated log if it exists (daemon.log.3 and beyond)
    // We only keep MAX_ROTATED_LOGS rotated files
    let oldest = rotated_path(log_path, MAX_ROTATED_LOGS + 1);
    let _ = fs::remove_file(&oldest);

    // Shift existing rotated logs: .3 <- .2, .2 <- .1
    for i in (1..=MAX_ROTATED_LOGS).rev() {
        let src = if i == 1 {
            // Special case: delete the target first so rename works
            rotated_path(log_path, i)
        } else {
            rotated_path(log_path, i)
        };
        let _ = fs::remove_file(&src); // remove target if it exists

        let prev = if i == 1 {
            log_path.to_path_buf()
        } else {
            rotated_path(log_path, i - 1)
        };

        if prev.exists() {
            let _ = fs::rename(&prev, &src);
        }
    }

    // After rotation, the main log_path should no longer exist (it was renamed to .1)
    // If it still exists for some reason, that's fine -- the daemon will append to it.
}

fn rotated_path(log_path: &Path, index: u32) -> std::path::PathBuf {
    let mut path = log_path.as_os_str().to_owned();
    path.push(format!(".{}", index));
    std::path::PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn does_not_rotate_when_under_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");

        fs::write(&log_path, "small content\n").unwrap();
        let size_before = fs::metadata(&log_path).unwrap().len();

        rotate_logs_if_needed(&log_path);

        // File should be unchanged
        let size_after = fs::metadata(&log_path).unwrap().len();
        assert_eq!(size_before, size_after);

        // No rotated files should exist
        assert!(!rotated_path(&log_path, 1).exists());
    }

    #[test]
    fn rotates_when_over_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");

        // Create a log file > 10MB
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let chunk = vec![b'x'; 1024 * 1024]; // 1MB
            for _ in 0..11 {
                f.write_all(&chunk).unwrap();
            }
        }

        assert!(fs::metadata(&log_path).unwrap().len() > MAX_LOG_SIZE);

        rotate_logs_if_needed(&log_path);

        // Original log should not exist (renamed to .1)
        assert!(!log_path.exists());
        // .1 should exist and be large
        let rotated_1 = rotated_path(&log_path, 1);
        assert!(rotated_1.exists());
        assert!(fs::metadata(&rotated_1).unwrap().len() > MAX_LOG_SIZE);
    }

    #[test]
    fn shifts_existing_rotated_files() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");

        // Create existing rotated files
        fs::write(rotated_path(&log_path, 1), "old .1 content").unwrap();
        fs::write(rotated_path(&log_path, 2), "old .2 content").unwrap();

        // Create a log file > 10MB
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let chunk = vec![b'y'; 1024 * 1024];
            for _ in 0..11 {
                f.write_all(&chunk).unwrap();
            }
        }

        rotate_logs_if_needed(&log_path);

        // .1 should now be the old main log (large)
        let rotated_1 = rotated_path(&log_path, 1);
        assert!(rotated_1.exists());
        assert!(fs::metadata(&rotated_1).unwrap().len() > MAX_LOG_SIZE);

        // .2 should be the old .1
        let rotated_2 = rotated_path(&log_path, 2);
        assert!(rotated_2.exists());
        assert_eq!(fs::read_to_string(&rotated_2).unwrap(), "old .1 content");

        // .3 should be the old .2
        let rotated_3 = rotated_path(&log_path, 3);
        assert!(rotated_3.exists());
        assert_eq!(fs::read_to_string(&rotated_3).unwrap(), "old .2 content");
    }

    #[test]
    fn deletes_files_beyond_max_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("daemon.log");

        // Create rotated files up to .3
        fs::write(rotated_path(&log_path, 1), "r1").unwrap();
        fs::write(rotated_path(&log_path, 2), "r2").unwrap();
        fs::write(rotated_path(&log_path, 3), "r3").unwrap();

        // Create a large main log
        {
            let mut f = fs::File::create(&log_path).unwrap();
            let chunk = vec![b'z'; 1024 * 1024];
            for _ in 0..11 {
                f.write_all(&chunk).unwrap();
            }
        }

        rotate_logs_if_needed(&log_path);

        // .3 should be the old .2 content (old .3 was deleted during shift)
        let rotated_3 = rotated_path(&log_path, 3);
        assert!(rotated_3.exists());
        assert_eq!(fs::read_to_string(&rotated_3).unwrap(), "r2");

        // .4 should not exist
        let rotated_4 = rotated_path(&log_path, 4);
        assert!(!rotated_4.exists());
    }

    #[test]
    fn handles_nonexistent_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nonexistent.log");

        // Should not panic
        rotate_logs_if_needed(&log_path);
    }
}
