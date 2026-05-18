#!/usr/bin/env python3
"""Parse integration test files and generate a Graphviz dot file documenting features.

Usage:
    python3 scripts/test_feature_tree.py > docs/feature_tree.dot
    dot -Tsvg docs/feature_tree.dot -o docs/feature_tree.svg
"""

import os
import re
import sys
from collections import defaultdict

TESTS_DIR = os.path.join(os.path.dirname(__file__), "..", "tests", "integration")


def extract_test_fns(filepath):
    """Extract #[test] function names from a Rust file."""
    fns = []
    with open(filepath, "r", errors="replace") as f:
        lines = f.readlines()
    for i, line in enumerate(lines):
        if line.strip() == "#[test]":
            for j in range(i + 1, min(i + 5, len(lines))):
                m = re.match(r"\s*fn\s+(\w+)", lines[j])
                if m:
                    fns.append(m.group(1))
                    break
    return fns


def module_to_category(module_name):
    """Map a test module name to a high-level feature category."""
    categories = {
        "attribution": [
            "agent_commits_blame", "ai_reflow_attribution", "background_agent_attribution",
            "bash_attribution", "initial_attributions", "virtual_attribution_unit",
            "formatting_non_substantial_ai_attribution", "pending_ai_edit_suppression",
            "sync_authorship_types", "range_authorship_unit",
        ],
        "blame": [
            "blame_comprehensive", "blame_flags", "blame_subdirectory",
        ],
        "checkpoint": [
            "checkpoint_debug_log", "checkpoint_explicit_paths", "checkpoint_perf",
            "checkpoint_size", "checkpoint_telemetry", "checkpoint_unit",
        ],
        "commit": [
            "post_commit_unit", "pre_commit_unit", "commit_post_stats_benchmark",
            "prompt_across_commit",
        ],
        "daemon": [
            "daemon_e2e", "daemon_lifecycle", "daemon_unit",
        ],
        "diff": [
            "diff", "diff_comprehensive", "diff_ignore_binary",
        ],
        "rebase": [
            "rebase", "rebase_attribution_remaining", "rebase_authorship_unit",
            "rebase_benchmark", "rebase_hooks_unit", "rebase_merge_commit_note_leak",
            "rebase_note_integrity", "rebase_realworld", "merge_rebase",
            "pull_rebase_ff",
        ],
        "stash": [
            "stash_attribution", "stash_hooks_unit",
        ],
        "telemetry": [
            "telemetry_e2e", "telemetry_queue_e2e", "session_event_repo_url",
        ],
        "transcripts": [
            "transcripts_claude_reader", "transcripts_e2e", "sweep_e2e",
        ],
        "agents": [
            "agent_presets_comprehensive", "agent_usage_repo_url", "agent_v1",
            "claude_code", "codex", "cursor", "droid", "firebender", "gemini",
            "github_copilot", "github_copilot_create_file", "github_copilot_integration",
            "github_copilot_tools", "ide_presets", "issue_1204_multi_agent",
            "opencode", "pi", "windsurf", "amp",
        ],
        "install": [
            "install_e2e", "install_hooks_comprehensive", "sublime_merge_installer",
            "jetbrains_download", "jetbrains_ide_types",
        ],
        "auth": [
            "auth_commands",
        ],
        "config": [
            "config_command", "config_pattern_detection", "gix_config_tests",
        ],
        "notes": [
            "fetch_notes", "push_hooks_comprehensive", "push_upstream_authorship",
            "notes_merge_mixed_fanout",
        ],
        "ci": [
            "ci_context_unit", "ci_handlers_comprehensive", "ci_local_skip_fetch",
            "ci_local_skip_push", "ci_partial_clone", "ci_squash_rebase",
        ],
        "merge": [
            "merge_authorship", "cherry_pick", "squash_merge",
        ],
        "git_operations": [
            "checkout_switch", "reset", "worktrees", "git_alias_resolution",
            "git_cli_arg_parsing", "graphite",
        ],
        "show": [
            "show_command", "show_prompt",
        ],
        "status": [
            "status_comprehensive", "status_ignore", "status_unit",
        ],
        "stats": [
            "stats", "stats_unit",
        ],
        "performance": [
            "performance", "performance_targets", "simple_benchmark",
            "bash_tool_benchmark", "secrets_benchmark",
        ],
        "sessions": [
            "sessions_and_prompts", "sessions_backwards_compat", "sessions_cutover",
            "stale_prompt_carry", "prompt_hash_migration", "prompt_utils_unit",
        ],
        "misc": [
            "debug_command", "dashboard_upgrade_commands", "e2e_user_scenarios",
            "real_world_workflows", "multi_repo_workspace", "cross_repo_cwd_attribution",
            "internal_machine_commands", "internal_spawn_safety",
            "non_utf8_files", "utf8_filenames", "chinese_text_edits",
            "subdirs", "simple_additions", "realistic_complex_edits",
            "e2big_post_filter", "tls_native_certs", "fast_reader",
            "ignore_prompts", "ignore_unit", "continue_cli",
            "git_repository_comprehensive", "github_integration",
            "repo_storage_unit", "repository_unit", "refs_unit",
            "test_utils_unit",
        ],
    }

    for category, modules in categories.items():
        if module_name in modules:
            return category
    return "misc"


def collect_test_data():
    test_modules = defaultdict(list)

    for fname in sorted(os.listdir(TESTS_DIR)):
        if not fname.endswith(".rs"):
            continue
        if fname in ("main.rs", "test_utils.rs"):
            continue

        module_name = fname[:-3]
        filepath = os.path.join(TESTS_DIR, fname)
        fns = extract_test_fns(filepath)
        if fns:
            category = module_to_category(module_name)
            test_modules[category].append((module_name, fns))

    return test_modules


def output_dot(test_modules):
    print("digraph features {")
    print('    rankdir=LR;')
    print('    node [shape=box, style=filled, fillcolor="#f0f0f0", fontname="Helvetica"];')
    print('    edge [color="#666666"];')
    print()
    print('    root [label="git-ai\\nFeature Tree", shape=ellipse, fillcolor="#4a90d9", fontcolor=white];')
    print()

    for category in sorted(test_modules.keys()):
        cat_id = f"cat_{category}"
        label = category.replace("_", " ").title()
        modules = test_modules[category]
        total_tests = sum(len(fns) for _, fns in modules)
        print(f'    {cat_id} [label="{label}\\n({total_tests} tests)", fillcolor="#b8d4f0"];')
        print(f"    root -> {cat_id};")
        print()

        for module_name, fns in modules:
            mod_id = f"mod_{module_name}"
            mod_label = module_name.replace("_", " ")
            print(f'    {mod_id} [label="{mod_label}\\n({len(fns)})", fontsize=10];')
            print(f"    {cat_id} -> {mod_id};")

        print()

    print("}")


def output_text(test_modules):
    total = sum(len(fns) for modules in test_modules.values() for _, fns in modules)
    print(f"git-ai Feature Tree ({total} e2e tests)")
    print("=" * 50)

    for category in sorted(test_modules.keys()):
        modules = test_modules[category]
        cat_total = sum(len(fns) for _, fns in modules)
        label = category.replace("_", " ").title()
        print(f"\n{label} ({cat_total} tests)")
        for module_name, fns in modules:
            mod_label = module_name.replace("_", " ")
            print(f"  {mod_label} ({len(fns)})")


def main():
    test_modules = collect_test_data()

    if "--text" in sys.argv:
        output_text(test_modules)
    else:
        output_dot(test_modules)


if __name__ == "__main__":
    main()
