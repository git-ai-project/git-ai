//! Best-effort filesystem dedupe for local telemetry throttling.

use crate::error::GitAiError;
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const RETENTION_DAYS: i64 = 5;
const CLEANUP_INTERVAL_SECS: u64 = 60 * 60 * 6;
const CACHE_MAX_ENTRIES: usize = 10_000;

static LAST_CLEANUP_TS: AtomicU64 = AtomicU64::new(0);
static DEDUPE_CACHE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn dedupe_cache() -> &'static Mutex<HashMap<String, u64>> {
    DEDUPE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_key(namespace: &str, hash: &str) -> String {
    format!("{namespace}:{hash}")
}

fn utc_day_from_ts(ts: u64) -> NaiveDate {
    Utc.timestamp_opt(ts as i64, 0)
        .single()
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| Utc::now().date_naive())
}

fn marker_path(base_dir: &Path, namespace: &str, day: &str, hash: &str) -> PathBuf {
    let fanout = &hash[..2];
    base_dir
        .join(namespace)
        .join(day)
        .join(fanout)
        .join(format!("{hash}.ts"))
}

fn day_buckets(now_ts: u64, ttl_secs: u64) -> Vec<String> {
    let now_day = utc_day_from_ts(now_ts);
    let prior_days = ttl_secs.div_ceil(86_400).saturating_add(1);

    (0..=prior_days)
        .map(|offset| {
            (now_day - Duration::days(offset as i64))
                .format("%Y-%m-%d")
                .to_string()
        })
        .collect()
}

fn write_marker(path: &Path, ts: u64) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    std::fs::write(&tmp_path, ts.to_string())?;
    if let Err(err) = replace_file_atomic(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err.into());
    }

    Ok(())
}

#[cfg(windows)]
fn replace_file_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) && to.exists() =>
        {
            let _ = std::fs::remove_file(to);
            std::fs::rename(from, to).map_err(|rename_err| {
                std::io::Error::new(
                    rename_err.kind(),
                    format!(
                        "failed to replace existing marker after initial rename error ({err}): {rename_err}"
                    ),
                )
            })
        }
        Err(err) => Err(err),
    }
}

#[cfg(not(windows))]
fn replace_file_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

fn put_cache(namespace: &str, hash: &str, ts: u64) {
    let Ok(mut cache) = dedupe_cache().lock() else {
        return;
    };

    if cache.len() >= CACHE_MAX_ENTRIES {
        let remove_count = CACHE_MAX_ENTRIES / 4;
        let keys_to_remove: Vec<String> = cache.keys().take(remove_count).cloned().collect();
        for key in keys_to_remove {
            cache.remove(&key);
        }
    }

    cache.insert(cache_key(namespace, hash), ts);
}

fn get_cache(namespace: &str, hash: &str) -> Option<u64> {
    let Ok(cache) = dedupe_cache().lock() else {
        return None;
    };
    cache.get(&cache_key(namespace, hash)).copied()
}

fn dedupe_base_dir() -> Result<PathBuf, GitAiError> {
    #[cfg(any(test, feature = "test-support"))]
    if let Ok(path) = std::env::var("GIT_AI_TEST_TELEMETRY_DEDUPE_DIR") {
        return Ok(PathBuf::from(path));
    }

    #[cfg(any(test, feature = "test-support"))]
    {
        Ok(std::env::temp_dir().join(format!("git-ai-telemetry-dedupe-{}", std::process::id())))
    }

    #[cfg(not(any(test, feature = "test-support")))]
    {
        let home = dirs::home_dir().ok_or_else(|| {
            GitAiError::Generic("Could not determine home directory for dedupe storage".to_string())
        })?;
        Ok(home
            .join(".git-ai")
            .join("internal")
            .join("telemetry-dedupe"))
    }
}

#[cfg(any(test, feature = "test-support"))]
#[allow(dead_code)]
pub(crate) fn reset_for_tests() {
    LAST_CLEANUP_TS.store(0, Ordering::Relaxed);
    if let Ok(mut cache) = dedupe_cache().lock() {
        cache.clear();
    }
}

pub(crate) fn maybe_cleanup(now_ts: u64) {
    let previous = LAST_CLEANUP_TS.load(Ordering::Relaxed);
    if previous > 0 && now_ts.saturating_sub(previous) < CLEANUP_INTERVAL_SECS {
        return;
    }

    if LAST_CLEANUP_TS
        .compare_exchange(previous, now_ts, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        let _ = cleanup_old_days(now_ts);
    }
}

pub(crate) fn cleanup_old_days(now_ts: u64) -> Result<(), GitAiError> {
    let base_dir = dedupe_base_dir()?;
    if !base_dir.exists() {
        return Ok(());
    }

    let today = utc_day_from_ts(now_ts);
    let keep_after = today - Duration::days(RETENTION_DAYS - 1);

    for namespace_entry in std::fs::read_dir(&base_dir)? {
        let namespace_entry = namespace_entry?;
        if !namespace_entry.file_type()?.is_dir() {
            continue;
        }

        for day_entry in std::fs::read_dir(namespace_entry.path())? {
            let day_entry = day_entry?;
            if !day_entry.file_type()?.is_dir() {
                continue;
            }

            let day_name = day_entry.file_name();
            let day_name = day_name.to_string_lossy();
            let Ok(day_date) = NaiveDate::parse_from_str(&day_name, "%Y-%m-%d") else {
                continue;
            };

            if day_date < keep_after {
                let _ = std::fs::remove_dir_all(day_entry.path());
            }
        }
    }

    Ok(())
}

pub(crate) fn should_emit(namespace: &str, key: &str, now_ts: u64, ttl_secs: u64) -> bool {
    if namespace.trim().is_empty() || key.trim().is_empty() {
        return true;
    }

    maybe_cleanup(now_ts);

    let base_dir = match dedupe_base_dir() {
        Ok(path) => path,
        Err(_) => return true,
    };

    let hash = key_hash(key);

    if ttl_secs > 0
        && let Some(previous_ts) = get_cache(namespace, &hash)
        && now_ts.saturating_sub(previous_ts) < ttl_secs
    {
        return false;
    }

    let mut latest_ts: Option<u64> = None;
    for day_bucket in day_buckets(now_ts, ttl_secs) {
        let path = marker_path(&base_dir, namespace, &day_bucket, &hash);
        if !path.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(value) => value,
            Err(_) => {
                let today = utc_day_from_ts(now_ts).format("%Y-%m-%d").to_string();
                let today_path = marker_path(&base_dir, namespace, &today, &hash);
                let _ = write_marker(&today_path, now_ts);
                put_cache(namespace, &hash, now_ts);
                return true;
            }
        };

        let ts = match content.trim().parse::<u64>() {
            Ok(value) => value,
            Err(_) => {
                let today = utc_day_from_ts(now_ts).format("%Y-%m-%d").to_string();
                let today_path = marker_path(&base_dir, namespace, &today, &hash);
                let _ = write_marker(&today_path, now_ts);
                put_cache(namespace, &hash, now_ts);
                return true;
            }
        };

        latest_ts = Some(latest_ts.map_or(ts, |current| current.max(ts)));
    }

    if ttl_secs > 0
        && let Some(previous_ts) = latest_ts
        && now_ts.saturating_sub(previous_ts) < ttl_secs
    {
        put_cache(namespace, &hash, previous_ts);
        return false;
    }

    let today = utc_day_from_ts(now_ts).format("%Y-%m-%d").to_string();
    let today_path = marker_path(&base_dir, namespace, &today, &hash);

    if write_marker(&today_path, now_ts).is_err() {
        return true;
    }

    put_cache(namespace, &hash, now_ts);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    struct EnvRestoreGuard {
        previous: Option<OsString>,
    }

    impl Drop for EnvRestoreGuard {
        fn drop(&mut self) {
            // SAFETY: tests are marked #[serial], so process env mutation is safe.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("GIT_AI_TEST_TELEMETRY_DEDUPE_DIR", value),
                    None => std::env::remove_var("GIT_AI_TEST_TELEMETRY_DEDUPE_DIR"),
                }
            }
        }
    }

    fn with_temp_dedupe_dir<F: FnOnce(&Path)>(f: F) {
        let temp = TempDir::new().unwrap();
        let dedupe_dir = temp.path().join("telemetry-dedupe");
        reset_for_tests();

        let _restore_guard = EnvRestoreGuard {
            previous: std::env::var_os("GIT_AI_TEST_TELEMETRY_DEDUPE_DIR"),
        };

        // SAFETY: tests are marked #[serial], so process env mutation is safe.
        unsafe {
            std::env::set_var("GIT_AI_TEST_TELEMETRY_DEDUPE_DIR", &dedupe_dir);
        }

        f(&dedupe_dir);
        reset_for_tests();
    }

    #[test]
    #[serial]
    fn test_should_emit_respects_ttl_window() {
        with_temp_dedupe_dir(|_| {
            let now = 1_700_000_000;
            assert!(should_emit("response_start", "key-1", now, 60));
            assert!(!should_emit("response_start", "key-1", now + 10, 60));
            assert!(should_emit("response_start", "key-1", now + 61, 60));
        });
    }

    #[test]
    #[serial]
    fn test_should_emit_namespace_isolation() {
        with_temp_dedupe_dir(|_| {
            let now = 1_700_000_000;
            assert!(should_emit("agent_usage", "same-key", now, 120));
            assert!(should_emit("response_start", "same-key", now + 1, 120));
            assert!(!should_emit("agent_usage", "same-key", now + 2, 120));
            assert!(!should_emit("response_start", "same-key", now + 3, 120));
        });
    }

    #[test]
    #[serial]
    fn test_should_emit_reads_previous_day_bucket_within_ttl() {
        with_temp_dedupe_dir(|_| {
            let first = Utc
                .with_ymd_and_hms(2026, 2, 24, 23, 59, 50)
                .single()
                .unwrap()
                .timestamp() as u64;
            let second = Utc
                .with_ymd_and_hms(2026, 2, 25, 0, 0, 10)
                .single()
                .unwrap()
                .timestamp() as u64;

            assert!(should_emit("response_end", "cross-day", first, 86_400 * 2));
            assert!(!should_emit(
                "response_end",
                "cross-day",
                second,
                86_400 * 2
            ));
        });
    }

    #[test]
    #[serial]
    fn test_should_emit_creates_hash_fanout_path() {
        with_temp_dedupe_dir(|base| {
            let now = 1_700_000_000;
            assert!(should_emit("session_start", "my-session", now, 60));

            let hash = key_hash("my-session");
            let day = utc_day_from_ts(now).format("%Y-%m-%d").to_string();
            let path = marker_path(base, "session_start", &day, &hash);
            assert!(path.exists());
            assert_eq!(
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|v| v.to_str()),
                Some(&hash[..2])
            );
        });
    }

    #[test]
    #[serial]
    fn test_should_emit_overwrites_existing_marker_file() {
        with_temp_dedupe_dir(|base| {
            let now = 1_700_000_000;
            assert!(should_emit("response_end", "overwrite", now, 0));
            assert!(should_emit("response_end", "overwrite", now + 1, 0));

            let hash = key_hash("overwrite");
            let day = utc_day_from_ts(now + 1).format("%Y-%m-%d").to_string();
            let path = marker_path(base, "response_end", &day, &hash);
            let ts = std::fs::read_to_string(path).unwrap();
            assert_eq!(ts.trim(), (now + 1).to_string());
        });
    }

    #[test]
    #[serial]
    fn test_malformed_timestamp_fails_open_and_self_heals() {
        with_temp_dedupe_dir(|base| {
            let now = 1_700_000_000;
            let hash = key_hash("broken");
            let day = utc_day_from_ts(now).format("%Y-%m-%d").to_string();
            let path = marker_path(base, "response_start", &day, &hash);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "not-a-timestamp").unwrap();

            assert!(should_emit("response_start", "broken", now, 10_000));
            let healed = std::fs::read_to_string(path).unwrap();
            assert_eq!(healed.trim(), now.to_string());
        });
    }

    #[test]
    #[serial]
    fn test_cleanup_old_days_removes_old_day_directories() {
        with_temp_dedupe_dir(|base| {
            let now = Utc
                .with_ymd_and_hms(2026, 2, 24, 12, 0, 0)
                .single()
                .unwrap()
                .timestamp() as u64;

            let old_day = (utc_day_from_ts(now) - Duration::days(8))
                .format("%Y-%m-%d")
                .to_string();
            let keep_day = (utc_day_from_ts(now) - Duration::days(2))
                .format("%Y-%m-%d")
                .to_string();

            std::fs::create_dir_all(base.join("agent_usage").join(&old_day).join("ab")).unwrap();
            std::fs::write(
                base.join("agent_usage")
                    .join(&old_day)
                    .join("ab")
                    .join("old.ts"),
                "1",
            )
            .unwrap();

            std::fs::create_dir_all(base.join("agent_usage").join(&keep_day).join("cd")).unwrap();
            std::fs::write(
                base.join("agent_usage")
                    .join(&keep_day)
                    .join("cd")
                    .join("keep.ts"),
                "1",
            )
            .unwrap();

            cleanup_old_days(now).unwrap();

            assert!(!base.join("agent_usage").join(old_day).exists());
            assert!(base.join("agent_usage").join(keep_day).exists());
        });
    }

    #[test]
    #[serial]
    fn test_large_volume_cleanup_is_day_based() {
        with_temp_dedupe_dir(|base| {
            let now = Utc
                .with_ymd_and_hms(2026, 2, 24, 12, 0, 0)
                .single()
                .unwrap()
                .timestamp() as u64;
            let old_ts = now - 86_400 * 10;

            for i in 0..3_000 {
                let key = format!("key-{i}");
                assert!(should_emit("response_end", &key, old_ts, 60));
            }

            cleanup_old_days(now).unwrap();

            let old_day = utc_day_from_ts(old_ts).format("%Y-%m-%d").to_string();
            assert!(!base.join("response_end").join(old_day).exists());
        });
    }

    #[test]
    #[serial]
    fn test_concurrent_should_emit_calls_do_not_panic() {
        with_temp_dedupe_dir(|_| {
            let thread_count = 32;
            let barrier = Arc::new(Barrier::new(thread_count));
            let mut handles = Vec::new();

            for idx in 0..thread_count {
                let barrier = Arc::clone(&barrier);
                handles.push(thread::spawn(move || {
                    barrier.wait();
                    for i in 0..200 {
                        let ts = 1_700_000_000 + i as u64;
                        let key = format!("shared-key-{}", i % 10);
                        let _ = should_emit("response_start", &key, ts + idx as u64, 30);
                    }
                }));
            }

            for handle in handles {
                handle.join().expect("thread should complete without panic");
            }
        });
    }
}
