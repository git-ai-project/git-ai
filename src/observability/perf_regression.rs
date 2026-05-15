use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Maximum number of samples to keep per operation in the rolling buffer.
const MAX_SAMPLES_PER_OP: usize = 1000;

/// Regression threshold multiplier: warn if elapsed > THRESHOLD * p95.
const REGRESSION_THRESHOLD: f64 = 2.0;

/// A warning produced when a timing exceeds the regression threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct RegressionWarning {
    pub operation: String,
    pub elapsed_ms: f64,
    pub baseline_p95_ms: f64,
    pub ratio: f64,
}

/// Baseline statistics for a single operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationBaseline {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub samples: usize,
}

/// The full baseline file: operation name -> stats.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerfBaseline(pub HashMap<String, OperationBaseline>);

/// The rolling sample buffer: operation name -> list of recent timings (ms).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PerfSamples(pub HashMap<String, Vec<f64>>);

/// In-memory buffer of timings collected during this process lifetime.
/// Flushed to disk on `flush_samples()`.
static IN_MEMORY_SAMPLES: OnceLock<Mutex<HashMap<String, Vec<f64>>>> = OnceLock::new();

fn samples_lock() -> &'static Mutex<HashMap<String, Vec<f64>>> {
    IN_MEMORY_SAMPLES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Directory for perf data files.
fn perf_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("GIT_AI_PERF_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".git-ai")
}

fn baseline_path() -> PathBuf {
    perf_data_dir().join("perf_baseline.json")
}

fn samples_path() -> PathBuf {
    perf_data_dir().join("perf_samples.json")
}

/// Record a timing for a given operation into the in-memory rolling buffer.
pub fn record_timing(operation: &str, elapsed_ms: f64) {
    if let Ok(mut map) = samples_lock().lock() {
        let samples = map.entry(operation.to_string()).or_default();
        samples.push(elapsed_ms);
        // Keep only the last MAX_SAMPLES_PER_OP entries
        if samples.len() > MAX_SAMPLES_PER_OP {
            let excess = samples.len() - MAX_SAMPLES_PER_OP;
            samples.drain(..excess);
        }
    }
}

/// Check if the given timing represents a performance regression relative to the baseline.
/// Returns `Some(RegressionWarning)` if elapsed > 2x the p95 baseline for that operation.
pub fn check_regression(operation: &str, elapsed_ms: f64) -> Option<RegressionWarning> {
    let baseline = load_baseline().ok()?;
    let op_baseline = baseline.0.get(operation)?;

    if op_baseline.p95_ms <= 0.0 {
        return None;
    }

    let ratio = elapsed_ms / op_baseline.p95_ms;
    if ratio > REGRESSION_THRESHOLD {
        Some(RegressionWarning {
            operation: operation.to_string(),
            elapsed_ms,
            baseline_p95_ms: op_baseline.p95_ms,
            ratio,
        })
    } else {
        None
    }
}

/// Capture baseline from current samples (both in-memory and on-disk).
/// Computes p50/p95 from the combined sample set and writes to `perf_baseline.json`.
pub fn capture_baseline() -> Result<(), String> {
    // First, flush in-memory samples to disk
    flush_samples().map_err(|e| format!("failed to flush samples: {}", e))?;

    // Load the on-disk samples
    let samples = load_samples().map_err(|e| format!("failed to load samples: {}", e))?;

    if samples.0.is_empty() {
        return Err("no samples available to compute baseline".to_string());
    }

    let mut baseline = PerfBaseline::default();

    for (op, timings) in &samples.0 {
        if timings.is_empty() {
            continue;
        }
        let mut sorted = timings.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let p50 = percentile(&sorted, 50.0);
        let p95 = percentile(&sorted, 95.0);

        baseline.0.insert(
            op.clone(),
            OperationBaseline {
                p50_ms: p50,
                p95_ms: p95,
                samples: sorted.len(),
            },
        );
    }

    // Write baseline
    let dir = perf_data_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create dir: {}", e))?;
    let json = serde_json::to_string_pretty(&baseline)
        .map_err(|e| format!("failed to serialize baseline: {}", e))?;
    fs::write(baseline_path(), json).map_err(|e| format!("failed to write baseline: {}", e))?;

    Ok(())
}

/// Flush the in-memory sample buffer to disk, merging with any existing samples.
pub fn flush_samples() -> Result<(), String> {
    let in_memory = {
        let mut lock = samples_lock()
            .lock()
            .map_err(|e| format!("lock poisoned: {}", e))?;

        std::mem::take(&mut *lock)
    };

    if in_memory.is_empty() {
        return Ok(());
    }

    // Load existing on-disk samples
    let mut on_disk = load_samples().unwrap_or_default();

    // Merge in-memory into on-disk
    for (op, timings) in in_memory {
        let entry = on_disk.0.entry(op).or_default();
        entry.extend(timings);
        // Trim to max
        if entry.len() > MAX_SAMPLES_PER_OP {
            let excess = entry.len() - MAX_SAMPLES_PER_OP;
            entry.drain(..excess);
        }
    }

    // Write back
    let dir = perf_data_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create dir: {}", e))?;
    let json = serde_json::to_string_pretty(&on_disk)
        .map_err(|e| format!("failed to serialize samples: {}", e))?;
    fs::write(samples_path(), json).map_err(|e| format!("failed to write samples: {}", e))?;

    Ok(())
}

/// Load the baseline from disk.
pub fn load_baseline() -> Result<PerfBaseline, String> {
    let path = baseline_path();
    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read baseline: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("failed to parse baseline: {}", e))
}

/// Load samples from disk.
pub fn load_samples() -> Result<PerfSamples, String> {
    let path = samples_path();
    if !path.exists() {
        return Ok(PerfSamples::default());
    }
    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read samples: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("failed to parse samples: {}", e))
}

/// Reset all perf data (baseline and samples), both on-disk and in-memory.
pub fn reset_all() -> Result<(), String> {
    // Clear in-memory
    if let Ok(mut lock) = samples_lock().lock() {
        lock.clear();
    }

    // Remove files
    let _ = fs::remove_file(baseline_path());
    let _ = fs::remove_file(samples_path());

    Ok(())
}

/// Compute a percentile value from a sorted slice.
/// Uses linear interpolation between adjacent ranks.
pub fn percentile(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }

    let rank = (pct / 100.0) * (sorted.len() as f64 - 1.0);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;

    if lower == upper {
        sorted[lower]
    } else {
        let frac = rank - lower as f64;
        sorted[lower] * (1.0 - frac) + sorted[upper] * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_percentile_basic() {
        // 10 samples: 1..=10
        let sorted: Vec<f64> = (1..=10).map(|x| x as f64).collect();
        let p50 = percentile(&sorted, 50.0);
        // rank = 0.5 * 9 = 4.5 => interpolate between sorted[4]=5 and sorted[5]=6 => 5.5
        assert!((p50 - 5.5).abs() < 0.001, "p50 = {}", p50);

        let p95 = percentile(&sorted, 95.0);
        // rank = 0.95 * 9 = 8.55 => interpolate between sorted[8]=9 and sorted[9]=10
        // = 9 * 0.45 + 10 * 0.55 = 4.05 + 5.5 = 9.55
        assert!((p95 - 9.55).abs() < 0.001, "p95 = {}", p95);
    }

    #[test]
    fn test_percentile_single_element() {
        assert_eq!(percentile(&[42.0], 50.0), 42.0);
        assert_eq!(percentile(&[42.0], 95.0), 42.0);
    }

    #[test]
    fn test_percentile_empty() {
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn test_percentile_two_elements() {
        let sorted = vec![2.0, 8.0];
        let p50 = percentile(&sorted, 50.0);
        // rank = 0.5 * 1 = 0.5 => 2*(1-0.5) + 8*0.5 = 1 + 4 = 5
        assert!((p50 - 5.0).abs() < 0.001, "p50 = {}", p50);
    }

    #[test]
    fn test_regression_detection_no_baseline() {
        // With no baseline loaded, should return None
        // (Since we can't easily mock the file path in unit tests, test the logic directly)
        let baseline = PerfBaseline::default();
        let result = check_regression_with_baseline(&baseline, "checkpoint", 10.0);
        assert_eq!(result, None);
    }

    #[test]
    fn test_regression_detection_below_threshold() {
        let mut baseline = PerfBaseline::default();
        baseline.0.insert(
            "checkpoint".to_string(),
            OperationBaseline {
                p50_ms: 2.0,
                p95_ms: 4.0,
                samples: 100,
            },
        );
        // 7.9ms / 4.0ms = 1.975 < 2.0 threshold
        let result = check_regression_with_baseline(&baseline, "checkpoint", 7.9);
        assert_eq!(result, None);
    }

    #[test]
    fn test_regression_detection_above_threshold() {
        let mut baseline = PerfBaseline::default();
        baseline.0.insert(
            "checkpoint".to_string(),
            OperationBaseline {
                p50_ms: 2.0,
                p95_ms: 4.0,
                samples: 100,
            },
        );
        // 8.1ms / 4.0ms = 2.025 > 2.0 threshold
        let result = check_regression_with_baseline(&baseline, "checkpoint", 8.1);
        assert!(result.is_some());
        let warning = result.unwrap();
        assert_eq!(warning.operation, "checkpoint");
        assert!((warning.elapsed_ms - 8.1).abs() < 0.001);
        assert!((warning.baseline_p95_ms - 4.0).abs() < 0.001);
        assert!((warning.ratio - 2.025).abs() < 0.001);
    }

    #[test]
    fn test_regression_detection_zero_baseline() {
        let mut baseline = PerfBaseline::default();
        baseline.0.insert(
            "checkpoint".to_string(),
            OperationBaseline {
                p50_ms: 0.0,
                p95_ms: 0.0,
                samples: 0,
            },
        );
        // Should not warn when baseline is zero
        let result = check_regression_with_baseline(&baseline, "checkpoint", 5.0);
        assert_eq!(result, None);
    }

    #[test]
    #[serial]
    fn test_record_timing_basic() {
        // Clear any existing state
        if let Ok(mut lock) = samples_lock().lock() {
            lock.clear();
        }

        record_timing("test_op", 1.5);
        record_timing("test_op", 2.3);
        record_timing("other_op", 0.8);

        let lock = samples_lock().lock().unwrap();
        assert_eq!(lock.get("test_op").map(|v| v.len()), Some(2));
        assert_eq!(lock.get("other_op").map(|v| v.len()), Some(1));
    }

    #[test]
    #[serial]
    fn test_record_timing_rolling_limit() {
        if let Ok(mut lock) = samples_lock().lock() {
            lock.clear();
        }

        // Insert more than MAX_SAMPLES_PER_OP
        for i in 0..1050 {
            record_timing("overflow_op", i as f64);
        }

        let lock = samples_lock().lock().unwrap();
        let samples = lock.get("overflow_op").unwrap();
        assert_eq!(samples.len(), MAX_SAMPLES_PER_OP);
        // Should have kept the last 1000 (indices 50..1050)
        assert!((samples[0] - 50.0).abs() < 0.001);
    }

    /// Internal helper for testing regression detection logic without file I/O.
    fn check_regression_with_baseline(
        baseline: &PerfBaseline,
        operation: &str,
        elapsed_ms: f64,
    ) -> Option<RegressionWarning> {
        let op_baseline = baseline.0.get(operation)?;

        if op_baseline.p95_ms <= 0.0 {
            return None;
        }

        let ratio = elapsed_ms / op_baseline.p95_ms;
        if ratio > REGRESSION_THRESHOLD {
            Some(RegressionWarning {
                operation: operation.to_string(),
                elapsed_ms,
                baseline_p95_ms: op_baseline.p95_ms,
                ratio,
            })
        } else {
            None
        }
    }
}
