pub mod api_client;
pub mod checkpoint_worker;
pub mod commit_detector;
pub mod control_client;
pub mod control_socket;
pub mod event_loop;
pub mod health;
pub mod lifecycle;
pub mod log_rotation;
pub mod post_commit_worker;
pub mod protocol;
pub mod repo_resolver;
pub mod rewrite_worker;
pub mod run;
pub mod service;
pub mod startup;
pub mod stash_worker;
pub mod stats;
pub mod stats_persistence;
pub mod telemetry_contract_tests;
pub mod telemetry_types;
pub mod telemetry_worker;
pub mod trace2_events;

#[cfg(unix)]
pub mod trace2_listener;

#[cfg(any(windows, test))]
pub mod trace2_listener_win;

pub use lifecycle::{DaemonPaths, DaemonPid};
pub use run::run_daemon;
