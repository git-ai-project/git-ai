#[macro_use]
#[allow(dead_code)]
mod repos;

#[allow(dead_code)]
mod test_utils;

// E2E tests — black-box attribution tests against v2 daemon architecture
mod agent_commits_blame;
mod ai_reflow_attribution;
mod amend;
mod background_agent_attribution;
mod bash_attribution;
mod blame_subdirectory;
mod checkpoint_debug_log;
mod checkpoint_explicit_paths;
mod checkpoint_size;
mod checkout_switch;
mod chinese_text_edits;
mod ci_local_skip_fetch;
mod ci_local_skip_push;
mod config_pattern_detection;
mod cross_repo_cwd_attribution;
mod diff_comprehensive;
mod diff_ignore_binary;
mod fetch_notes;
mod formatting_non_substantial_ai_attribution;
mod github_copilot_create_file;
mod github_copilot_integration;
mod github_copilot_tools;
mod github_integration;
mod internal_machine_commands;
mod internal_spawn_safety;
mod issue_1204_multi_agent;
mod merge_rebase;
mod notes_merge_mixed_fanout;
mod pending_ai_edit_suppression;
mod prompt_hash_migration;
mod pull_rebase_ff;
mod push_upstream_authorship;
mod real_world_workflows;
mod realistic_complex_edits;
mod rebase;
mod rebase_attribution_remaining;
mod reset;
mod simple_additions;
mod simple_benchmark;
mod stash_attribution;
mod stash_hooks_unit;

#[cfg(unix)]
mod daemon_e2e;
#[cfg(unix)]
mod telemetry_e2e;
