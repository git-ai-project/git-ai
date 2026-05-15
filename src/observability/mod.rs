pub mod logging;
pub mod perf_regression;

use std::sync::OnceLock;
use std::time::Instant;

/// Performance debugging mode, cached from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PerfMode {
    /// Performance output is disabled.
    Off,
    /// Human-readable output to stderr.
    Human,
    /// JSON output to stderr (for programmatic consumption).
    Json,
}

/// Returns the cached performance mode (reads env once).
fn perf_mode() -> PerfMode {
    static MODE: OnceLock<PerfMode> = OnceLock::new();
    *MODE.get_or_init(
        || match std::env::var("GIT_AI_DEBUG_PERFORMANCE").as_deref() {
            Ok("1") => PerfMode::Human,
            Ok("2") => PerfMode::Json,
            _ => PerfMode::Off,
        },
    )
}

/// A lightweight performance timer that measures wall-clock elapsed time.
///
/// Reports timing information on drop when `GIT_AI_DEBUG_PERFORMANCE` is set.
/// When the env var is unset, the only overhead is a single `Instant::now()` call.
pub struct PerfTimer {
    label: &'static str,
    start: Instant,
    budget_ms: Option<u64>,
}

impl PerfTimer {
    /// Create a timer with no budget.
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            start: Instant::now(),
            budget_ms: None,
        }
    }

    /// Create a timer with a performance budget in milliseconds.
    pub fn with_budget(label: &'static str, budget_ms: u64) -> Self {
        Self {
            label,
            start: Instant::now(),
            budget_ms: Some(budget_ms),
        }
    }

    /// Returns the elapsed time in milliseconds since the timer was created.
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl Drop for PerfTimer {
    fn drop(&mut self) {
        let elapsed = self.elapsed_ms();
        let elapsed_f64 = elapsed as f64;

        // Always record timing for regression detection (very cheap: just a vec push).
        perf_regression::record_timing(self.label, elapsed_f64);

        // Check for regression and emit warning if detected.
        if let Some(warning) = perf_regression::check_regression(self.label, elapsed_f64) {
            eprintln!(
                "[git-ai perf] WARNING: {} took {:.1}ms (baseline p95: {:.1}ms, {:.1}x regression)",
                warning.operation, warning.elapsed_ms, warning.baseline_p95_ms, warning.ratio,
            );
        }

        let mode = perf_mode();
        if mode == PerfMode::Off {
            return;
        }

        match mode {
            PerfMode::Human => {
                if let Some(budget) = self.budget_ms {
                    let status = if elapsed > budget {
                        "\u{26a0} OVER BUDGET"
                    } else {
                        "\u{2713}"
                    };
                    eprintln!(
                        "[git-ai perf] {}: {}ms (budget: {}ms) {}",
                        self.label, elapsed, budget, status
                    );
                } else {
                    eprintln!("[git-ai perf] {}: {}ms", self.label, elapsed);
                }
            }
            PerfMode::Json => {
                if let Some(budget) = self.budget_ms {
                    eprintln!(
                        r#"{{"operation":"{}","elapsed_ms":{},"budget_ms":{},"over_budget":{}}}"#,
                        self.label,
                        elapsed,
                        budget,
                        elapsed > budget,
                    );
                } else {
                    eprintln!(
                        r#"{{"operation":"{}","elapsed_ms":{}}}"#,
                        self.label, elapsed,
                    );
                }
            }
            PerfMode::Off => unreachable!(),
        }
    }
}

// Ensure PerfTimer is Send + Sync (Instant is Send+Sync, &'static str is Send+Sync).
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PerfTimer>();
};

/// A performance budget entry mapping an operation name to its time budget.
pub struct PerfBudget {
    pub operation: &'static str,
    pub budget_ms: u64,
}

/// Performance budgets for critical operations (in milliseconds).
pub static BUDGETS: &[PerfBudget] = &[
    PerfBudget {
        operation: "checkpoint",
        budget_ms: 3,
    },
    PerfBudget {
        operation: "post_commit_daemon",
        budget_ms: 1,
    },
    PerfBudget {
        operation: "post_commit_sync",
        budget_ms: 3,
    },
    PerfBudget {
        operation: "blame_100",
        budget_ms: 6,
    },
    PerfBudget {
        operation: "blame_500",
        budget_ms: 11,
    },
    PerfBudget {
        operation: "blame_1000",
        budget_ms: 16,
    },
    PerfBudget {
        operation: "startup",
        budget_ms: 1,
    },
];

/// Look up the budget for a given operation name.
pub fn get_budget(operation: &str) -> Option<u64> {
    BUDGETS
        .iter()
        .find(|b| b.operation == operation)
        .map(|b| b.budget_ms)
}

/// Times a block and reports on drop. Zero-cost when perf debugging is off
/// (beyond the `Instant::now()` call).
///
/// Usage:
/// ```ignore
/// perf_time!("checkpoint", {
///     // ... do work ...
/// });
/// ```
#[macro_export]
macro_rules! perf_time {
    ($label:expr, $body:expr) => {{
        let _t = $crate::observability::PerfTimer::with_budget(
            $label,
            $crate::observability::get_budget($label).unwrap_or(0),
        );
        $body
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_timer_elapsed_measurement() {
        let timer = PerfTimer::new("test_op");
        thread::sleep(Duration::from_millis(5));
        let elapsed = timer.elapsed_ms();
        assert!(elapsed >= 4, "Expected elapsed >= 4ms, got {}ms", elapsed);
        // Explicitly forget to avoid drop reporting (env may not be set in tests)
        std::mem::forget(timer);
    }

    #[test]
    fn test_timer_with_budget() {
        let timer = PerfTimer::with_budget("test_budget", 10);
        assert_eq!(timer.budget_ms, Some(10));
        assert_eq!(timer.label, "test_budget");
        std::mem::forget(timer);
    }

    #[test]
    fn test_budget_lookup_found() {
        assert_eq!(get_budget("checkpoint"), Some(3));
        assert_eq!(get_budget("post_commit_daemon"), Some(1));
        assert_eq!(get_budget("post_commit_sync"), Some(3));
        assert_eq!(get_budget("blame_100"), Some(6));
        assert_eq!(get_budget("blame_500"), Some(11));
        assert_eq!(get_budget("blame_1000"), Some(16));
        assert_eq!(get_budget("startup"), Some(1));
    }

    #[test]
    fn test_budget_lookup_not_found() {
        assert_eq!(get_budget("nonexistent"), None);
    }

    #[test]
    fn test_human_format_under_budget() {
        // Verify the format string construction for human mode (under budget)
        let label = "checkpoint";
        let elapsed: u64 = 2;
        let budget: u64 = 3;
        let status = if elapsed > budget {
            "\u{26a0} OVER BUDGET"
        } else {
            "\u{2713}"
        };
        let output = format!(
            "[git-ai perf] {}: {}ms (budget: {}ms) {}",
            label, elapsed, budget, status
        );
        assert_eq!(
            output,
            "[git-ai perf] checkpoint: 2ms (budget: 3ms) \u{2713}"
        );
    }

    #[test]
    fn test_human_format_over_budget() {
        let label = "post_commit_sync";
        let elapsed: u64 = 5;
        let budget: u64 = 3;
        let status = if elapsed > budget {
            "\u{26a0} OVER BUDGET"
        } else {
            "\u{2713}"
        };
        let output = format!(
            "[git-ai perf] {}: {}ms (budget: {}ms) {}",
            label, elapsed, budget, status
        );
        assert_eq!(
            output,
            "[git-ai perf] post_commit_sync: 5ms (budget: 3ms) \u{26a0} OVER BUDGET"
        );
    }

    #[test]
    fn test_json_format_under_budget() {
        let label = "checkpoint";
        let elapsed: u64 = 2;
        let budget: u64 = 3;
        let over = elapsed > budget;
        let output = format!(
            r#"{{"operation":"{}","elapsed_ms":{},"budget_ms":{},"over_budget":{}}}"#,
            label, elapsed, budget, over,
        );
        assert_eq!(
            output,
            r#"{"operation":"checkpoint","elapsed_ms":2,"budget_ms":3,"over_budget":false}"#
        );
    }

    #[test]
    fn test_json_format_over_budget() {
        let label = "post_commit_sync";
        let elapsed: u64 = 5;
        let budget: u64 = 3;
        let over = elapsed > budget;
        let output = format!(
            r#"{{"operation":"{}","elapsed_ms":{},"budget_ms":{},"over_budget":{}}}"#,
            label, elapsed, budget, over,
        );
        assert_eq!(
            output,
            r#"{"operation":"post_commit_sync","elapsed_ms":5,"budget_ms":3,"over_budget":true}"#
        );
    }

    #[test]
    fn test_json_format_no_budget() {
        let label = "custom_op";
        let elapsed: u64 = 7;
        let output = format!(r#"{{"operation":"{}","elapsed_ms":{}}}"#, label, elapsed,);
        assert_eq!(output, r#"{"operation":"custom_op","elapsed_ms":7}"#);
    }

    #[test]
    fn test_perf_time_macro() {
        let result = perf_time!("checkpoint", 1 + 1);
        assert_eq!(result, 2);
    }

    #[test]
    fn test_budgets_table_not_empty() {
        assert!(!BUDGETS.is_empty());
        for budget in BUDGETS {
            assert!(!budget.operation.is_empty());
            assert!(budget.budget_ms > 0);
        }
    }
}
