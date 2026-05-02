use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Global flag to enable subprocess instrumentation
static INSTRUMENTATION_ENABLED: AtomicBool = AtomicBool::new(false);

/// Global metrics collector
static METRICS: OnceLock<Mutex<SubprocessMetrics>> = OnceLock::new();

/// Categories of subprocess operations for analysis
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubprocessCategory {
    /// Git operations during checkpoint processing
    Checkpoint,
    /// Git operations during commit hooks (pre/post)
    CommitHook,
    /// Git operations during rebase/cherry-pick hooks
    RebaseHook,
    /// Git operations for status/diff display
    StatusDisplay,
    /// Git operations for blame display
    BlameDisplay,
    /// Git operations for log display
    LogDisplay,
    /// Git operations for authorship note management
    AuthorshipNotes,
    /// Git operations for repository queries (rev-parse, show-ref, etc.)
    RepositoryQuery,
    /// Git operations for tree/object reading
    ObjectRead,
    /// Git operations for diff generation/parsing
    DiffOperation,
    /// Git operations for ref updates
    RefUpdate,
    /// Git daemon background operations
    DaemonBackground,
    /// Uncategorized git operations
    Other,
}

impl SubprocessCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::CommitHook => "commit_hook",
            Self::RebaseHook => "rebase_hook",
            Self::StatusDisplay => "status_display",
            Self::BlameDisplay => "blame_display",
            Self::LogDisplay => "log_display",
            Self::AuthorshipNotes => "authorship_notes",
            Self::RepositoryQuery => "repository_query",
            Self::ObjectRead => "object_read",
            Self::DiffOperation => "diff_operation",
            Self::RefUpdate => "ref_update",
            Self::DaemonBackground => "daemon_background",
            Self::Other => "other",
        }
    }
}

/// Context about where and why a subprocess is being spawned
#[derive(Debug, Clone)]
pub struct SubprocessContext {
    /// Category of operation
    pub category: SubprocessCategory,
    /// The git subcommand being invoked (e.g., "rev-parse", "show-ref")
    pub git_command: String,
    /// Whether this is on the critical path (user-blocking)
    pub critical_path: bool,
    /// Whether this is a read-only operation (candidate for caching)
    pub read_only: bool,
    /// Optional label for grouping related calls
    pub operation_label: Option<String>,
    /// Stack depth (for detecting nested calls)
    pub stack_depth: usize,
}

impl SubprocessContext {
    pub fn new(category: SubprocessCategory, git_command: &str) -> Self {
        Self {
            category,
            git_command: git_command.to_string(),
            critical_path: false,
            read_only: true,
            operation_label: None,
            stack_depth: 0,
        }
    }

    pub fn critical_path(mut self, critical: bool) -> Self {
        self.critical_path = critical;
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.operation_label = Some(label.into());
        self
    }

    pub fn stack_depth(mut self, depth: usize) -> Self {
        self.stack_depth = depth;
        self
    }
}

/// Metrics for a specific subprocess invocation
#[derive(Debug, Clone)]
pub struct SubprocessInvocation {
    pub context: SubprocessContext,
    pub duration: Duration,
    pub timestamp: Instant,
    pub args: Vec<String>,
}

/// Aggregated metrics for subprocess operations
#[derive(Debug)]
pub struct SubprocessMetrics {
    /// Total number of subprocess calls
    pub total_calls: u64,
    /// Calls by category
    pub calls_by_category: HashMap<SubprocessCategory, u64>,
    /// Calls by git command
    pub calls_by_command: HashMap<String, u64>,
    /// Total time spent in subprocesses
    pub total_duration: Duration,
    /// Individual invocations (for detailed analysis)
    pub invocations: Vec<SubprocessInvocation>,
    /// Start time of metrics collection
    pub start_time: Instant,
}

impl Default for SubprocessMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl SubprocessMetrics {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            total_calls: 0,
            calls_by_category: HashMap::new(),
            calls_by_command: HashMap::new(),
            total_duration: Duration::ZERO,
            invocations: Vec::new(),
        }
    }

    pub fn record(&mut self, invocation: SubprocessInvocation) {
        self.total_calls += 1;
        self.total_duration += invocation.duration;

        *self
            .calls_by_category
            .entry(invocation.context.category)
            .or_insert(0) += 1;

        *self
            .calls_by_command
            .entry(invocation.context.git_command.clone())
            .or_insert(0) += 1;

        self.invocations.push(invocation);
    }

    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Generate a summary report
    pub fn summary(&self) -> String {
        let mut report = String::new();

        report.push_str(&format!("\n=== Subprocess Instrumentation Report ===\n"));
        report.push_str(&format!("Total time elapsed: {:?}\n", self.elapsed()));
        report.push_str(&format!("Total subprocess calls: {}\n", self.total_calls));
        report.push_str(&format!(
            "Total time in subprocesses: {:?}\n",
            self.total_duration
        ));

        if self.total_calls > 0 {
            let avg_duration = self.total_duration / self.total_calls as u32;
            report.push_str(&format!(
                "Average subprocess duration: {:?}\n",
                avg_duration
            ));

            let subprocess_ratio =
                (self.total_duration.as_secs_f64() / self.elapsed().as_secs_f64() * 100.0) as u32;
            report.push_str(&format!(
                "Time spent in subprocesses: {}%\n",
                subprocess_ratio
            ));
        }

        report.push_str(&format!("\n--- Calls by Category ---\n"));
        let mut by_category: Vec<_> = self.calls_by_category.iter().collect();
        by_category.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (category, count) in by_category {
            let pct = (*count as f64 / self.total_calls as f64 * 100.0) as u32;
            report.push_str(&format!(
                "  {:20} {:6} ({}%)\n",
                category.as_str(),
                count,
                pct
            ));
        }

        report.push_str(&format!("\n--- Calls by Git Command ---\n"));
        let mut by_command: Vec<_> = self.calls_by_command.iter().collect();
        by_command.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (command, count) in by_command.iter().take(20) {
            let pct = (**count as f64 / self.total_calls as f64 * 100.0) as u32;
            report.push_str(&format!("  {:25} {:6} ({}%)\n", command, count, pct));
        }

        // Critical path analysis
        let critical_path_calls: u64 = self
            .invocations
            .iter()
            .filter(|inv| inv.context.critical_path)
            .count() as u64;

        if critical_path_calls > 0 {
            report.push_str(&format!("\n--- Critical Path Analysis ---\n"));
            report.push_str(&format!(
                "Critical path calls: {} ({}%)\n",
                critical_path_calls,
                (critical_path_calls as f64 / self.total_calls as f64 * 100.0) as u32
            ));

            let critical_duration: Duration = self
                .invocations
                .iter()
                .filter(|inv| inv.context.critical_path)
                .map(|inv| inv.duration)
                .sum();

            report.push_str(&format!(
                "Critical path time: {:?} ({}%)\n",
                critical_duration,
                (critical_duration.as_secs_f64() / self.total_duration.as_secs_f64() * 100.0)
                    as u32
            ));
        }

        // Batching opportunities
        report.push_str(&format!("\n--- Potential Batching Opportunities ---\n"));
        let mut sequential_groups: HashMap<
            (SubprocessCategory, String),
            Vec<&SubprocessInvocation>,
        > = HashMap::new();

        // Group calls that happen within 10ms of each other (likely sequential)
        for i in 0..self.invocations.len() {
            if i > 0 {
                let prev = &self.invocations[i - 1];
                let curr = &self.invocations[i];
                let time_diff = curr.timestamp.duration_since(prev.timestamp);

                if time_diff < Duration::from_millis(10)
                    && curr.context.category == prev.context.category
                {
                    let key = (curr.context.category, curr.context.git_command.clone());
                    sequential_groups.entry(key).or_default().push(curr);
                }
            }
        }

        let mut batch_candidates: Vec<_> = sequential_groups.iter().collect();
        batch_candidates.sort_by_key(|(_, invocations)| std::cmp::Reverse(invocations.len()));

        for ((category, command), invocations) in batch_candidates.iter().take(10) {
            if invocations.len() >= 3 {
                report.push_str(&format!(
                    "  {:20} {} (sequential {} calls)\n",
                    category.as_str(),
                    command,
                    invocations.len()
                ));
            }
        }

        report.push_str(&format!("\n=========================================\n"));

        report
    }

    /// Generate detailed JSON report for external analysis
    pub fn json_report(&self) -> String {
        use std::fmt::Write;

        let mut json = String::new();
        writeln!(json, "{{").unwrap();
        writeln!(json, "  \"total_calls\": {},", self.total_calls).unwrap();
        writeln!(
            json,
            "  \"total_duration_ms\": {},",
            self.total_duration.as_millis()
        )
        .unwrap();
        writeln!(json, "  \"elapsed_ms\": {},", self.elapsed().as_millis()).unwrap();

        writeln!(json, "  \"by_category\": {{").unwrap();
        let mut first = true;
        for (category, count) in &self.calls_by_category {
            if !first {
                writeln!(json, ",").unwrap();
            }
            write!(json, "    \"{}\": {}", category.as_str(), count).unwrap();
            first = false;
        }
        writeln!(json, "\n  }},").unwrap();

        writeln!(json, "  \"by_command\": {{").unwrap();
        first = true;
        for (command, count) in &self.calls_by_command {
            if !first {
                writeln!(json, ",").unwrap();
            }
            write!(json, "    \"{}\": {}", command, count).unwrap();
            first = false;
        }
        writeln!(json, "\n  }},").unwrap();

        writeln!(json, "  \"invocations\": [").unwrap();
        for (i, inv) in self.invocations.iter().enumerate() {
            if i > 0 {
                writeln!(json, ",").unwrap();
            }
            write!(json, "    {{").unwrap();
            write!(
                json,
                "\"category\": \"{}\", ",
                inv.context.category.as_str()
            )
            .unwrap();
            write!(json, "\"command\": \"{}\", ", inv.context.git_command).unwrap();
            write!(json, "\"duration_ms\": {}, ", inv.duration.as_millis()).unwrap();
            write!(json, "\"critical_path\": {}, ", inv.context.critical_path).unwrap();
            write!(json, "\"read_only\": {}", inv.context.read_only).unwrap();
            if let Some(label) = &inv.context.operation_label {
                write!(json, ", \"label\": \"{}\"", label).unwrap();
            }
            write!(json, "}}").unwrap();
        }
        writeln!(json, "\n  ]").unwrap();

        writeln!(json, "}}").unwrap();

        json
    }
}

/// Enable subprocess instrumentation
pub fn enable_instrumentation() {
    INSTRUMENTATION_ENABLED.store(true, Ordering::SeqCst);
    METRICS.get_or_init(|| Mutex::new(SubprocessMetrics::new()));
}

/// Check if instrumentation is enabled
pub fn is_enabled() -> bool {
    INSTRUMENTATION_ENABLED.load(Ordering::SeqCst)
}

/// Record a subprocess invocation
pub fn record_subprocess(context: SubprocessContext, duration: Duration, args: &[String]) {
    if !is_enabled() {
        return;
    }

    let invocation = SubprocessInvocation {
        context,
        duration,
        timestamp: Instant::now(),
        args: args.to_vec(),
    };

    if let Some(metrics) = METRICS.get() {
        if let Ok(mut metrics) = metrics.lock() {
            metrics.record(invocation);
        }
    }
}

/// Get current metrics snapshot
pub fn get_metrics() -> Option<SubprocessMetrics> {
    METRICS.get()?.lock().ok().map(|m| SubprocessMetrics {
        total_calls: m.total_calls,
        calls_by_category: m.calls_by_category.clone(),
        calls_by_command: m.calls_by_command.clone(),
        total_duration: m.total_duration,
        invocations: m.invocations.clone(),
        start_time: m.start_time,
    })
}

/// Print summary report to stderr
pub fn print_summary() {
    if let Some(metrics) = get_metrics() {
        eprintln!("{}", metrics.summary());
    }
}

/// Print JSON report to stderr
pub fn print_json() {
    if let Some(metrics) = get_metrics() {
        eprintln!("{}", metrics.json_report());
    }
}

/// Reset metrics (useful for testing)
#[cfg(test)]
pub fn reset_metrics() {
    if let Some(metrics) = METRICS.get() {
        if let Ok(mut metrics) = metrics.lock() {
            *metrics = SubprocessMetrics::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_collection() {
        reset_metrics();
        enable_instrumentation();

        let ctx = SubprocessContext::new(SubprocessCategory::RepositoryQuery, "rev-parse")
            .critical_path(true);

        record_subprocess(
            ctx,
            Duration::from_millis(10),
            &["rev-parse".to_string(), "HEAD".to_string()],
        );

        let metrics = get_metrics().unwrap();
        assert_eq!(metrics.total_calls, 1);
        assert_eq!(
            metrics.calls_by_category[&SubprocessCategory::RepositoryQuery],
            1
        );
    }

    #[test]
    fn test_summary_generation() {
        reset_metrics();
        enable_instrumentation();

        for i in 0..5 {
            let ctx =
                SubprocessContext::new(SubprocessCategory::Checkpoint, "diff").critical_path(i < 2);
            record_subprocess(ctx, Duration::from_millis(5), &["diff".to_string()]);
        }

        let metrics = get_metrics().unwrap();
        let summary = metrics.summary();

        assert!(summary.contains("Total subprocess calls: 5"));
        assert!(summary.contains("checkpoint"));
    }
}
