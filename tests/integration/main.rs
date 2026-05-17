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
mod blame_comprehensive;
mod blame_flags;
mod blame_subdirectory;
mod checkout_switch;
mod checkpoint_debug_log;
mod checkpoint_explicit_paths;
mod checkpoint_size;
mod cherry_pick;
mod chinese_text_edits;
mod ci_local_skip_fetch;
mod ci_local_skip_push;
mod config_pattern_detection;
mod cross_repo_cwd_attribution;
mod diff_comprehensive;
mod diff_ignore_binary;
mod e2e_user_scenarios;
mod fetch_notes;
mod formatting_non_substantial_ai_attribution;
mod github_copilot_create_file;
mod github_copilot_integration;
mod github_copilot_tools;
mod github_integration;
mod ide_presets;
mod internal_machine_commands;
mod internal_spawn_safety;
mod issue_1204_multi_agent;
mod merge_rebase;
mod multi_repo_workspace;
mod non_utf8_files;
mod notes_merge_mixed_fanout;
mod pending_ai_edit_suppression;
mod prompt_hash_migration;
mod pull_rebase_ff;
mod push_upstream_authorship;
mod real_world_workflows;
mod realistic_complex_edits;
mod rebase;
mod rebase_attribution_remaining;
mod rebase_benchmark;
mod rebase_note_integrity;
mod rebase_realworld;
mod reset;
mod sessions_and_prompts;
mod simple_additions;
mod simple_benchmark;
mod squash_merge;
mod stash_attribution;
mod stash_hooks_unit;
mod stats;
mod status_comprehensive;
mod subdirs;
mod utf8_filenames;
mod worktrees;

// --- New feature e2e tests ---
mod auth_commands;
mod config_command;
mod dashboard_upgrade_commands;
mod debug_command;
mod show_command;

// --- Coverage gap tests ---
mod merge_authorship;
mod push_hooks_comprehensive;

// --- Batch 1: tests with minimal/no v1-internal imports ---
mod checkpoint_perf;
mod pi;
mod post_commit_unit;
mod pre_commit_unit;
mod prompt_across_commit;
mod rebase_merge_commit_note_leak;
mod sessions_backwards_compat;
mod stale_prompt_carry;

#[cfg(unix)]
mod daemon_e2e;
#[cfg(unix)]
mod daemon_lifecycle;
#[cfg(unix)]
mod install_e2e;
#[cfg(unix)]
mod telemetry_e2e;
