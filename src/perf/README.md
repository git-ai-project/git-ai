This directory contains tools for measuring performance,
specifically the overhead of shelling out to subprocesses,
which is especially slow on Windows.

The `mod.rs` file is a Rust module that wraps the execution of
subprocesses executed via `Command::output` and `Command::status`,
via the `measured_output` and `measured_stats` wrappers.

NOTE: The act of measuring a command's execution time does not happen
automatically for all invocations of `Command::output` and `Command::status`.
You must consciously make the decision to replace `.output()` and `.status()`
with `.measured_output()` and `.measured_status()`,
which allows you to omit measurements that may be irrelevant, such as in tests.

The wrappers are implemented to be as low-overhead as possible when
logging is not enabled.
To enable logging, set the `GITAI_MEASURE_COMMAND_PERF` environment variable.
Logs will be written to stderr.

To analyze a log manually, pipe stderr to a file and pass that file's name
to `analyze_log.py`, e.g. `python3 analyze_log.py foo.stderr`.

To perform an analysis without needing to save the logged output manually,
use `run_analysis.py`, e.g. `python3 run_analysis.py cargo run diff @..f296d`.
This will additionally measure the overall execution time and print the
percentage of time spent within spawned commands.

For more fine-grained interactive profiling, the samply tool is recommended:
https://github.com/mstange/samply .
It supports both Windows and Linux.
Example invocation: `samply record target/debug/git-ai diff @..f296d`
Once a sample has been recorded, it can be loaded into an interactive view
in any web browser that provides collapsible sample trees, flamegraphs,
and stack charts.
