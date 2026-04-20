# Sessions V2 Stats Processing - Remove Legacy Prompt Fields

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `mixed_additions`, `total_ai_additions`, and `total_ai_deletions` from `CommitStats` and `ToolModelHeadlineStats`, eliminate the legacy prompt-based stats accumulation loop, and update all display/telemetry code to reflect that these fields no longer exist. Sessions don't track these metrics — stats are now derived purely from diff-based attribution.

**Architecture:** The stats module currently has two code paths in `stats_from_authorship_log`: one for prompts (accumulates `total_additions`, `total_deletions`, `overriden_lines`) and one for sessions (only computes waiting time). Since all new data uses sessions, we remove the prompt stats fields entirely from the output structs and all consumers. The `mixed` display row in markdown disappears. The "lines generated" ratio in markdown disappears. The telemetry in `post_commit.rs` stops sending these fields. The prompt loop stays for backwards compat (waiting time calculation) but no longer accumulates removed fields.

**Tech Stack:** Rust, insta (snapshot testing), serde

---

### Task 1: Remove `mixed_additions`, `total_ai_additions`, `total_ai_deletions` from Stats Structs

**Files:**
- Modify: `src/authorship/stats.rs:10-50`

- [ ] **Step 1: Remove the three fields from `ToolModelHeadlineStats`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolModelHeadlineStats {
    #[serde(default)]
    pub ai_additions: u32,
    #[serde(default)]
    pub ai_accepted: u32,
    #[serde(default)]
    pub time_waiting_for_ai: u64,
}
```

- [ ] **Step 2: Remove the three fields from `CommitStats`**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommitStats {
    #[serde(default)]
    pub human_additions: u32,
    #[serde(default)]
    pub unknown_additions: u32,
    #[serde(default)]
    pub ai_additions: u32,
    #[serde(default)]
    pub ai_accepted: u32,
    #[serde(default)]
    pub time_waiting_for_ai: u64,
    #[serde(default)]
    pub git_diff_deleted_lines: u32,
    #[serde(default)]
    pub git_diff_added_lines: u32,
    #[serde(default)]
    pub tool_model_breakdown: BTreeMap<String, ToolModelHeadlineStats>,
}
```

- [ ] **Step 3: Attempt to build to see all compilation errors**

Run: `task build 2>&1 | head -80`
Expected: FAIL with many compilation errors pointing to all consumers of removed fields. These will guide Tasks 2-5.

---

### Task 2: Update `stats_from_authorship_log` Function

**Files:**
- Modify: `src/authorship/stats.rs:440-541`

- [ ] **Step 1: Remove prompt stats accumulation and mixed capping logic**

The function should become:

```rust
pub fn stats_from_authorship_log(
    authorship_log: Option<&crate::authorship::authorship_log_serialization::AuthorshipLog>,
    git_diff_added_lines: u32,
    git_diff_deleted_lines: u32,
    ai_accepted: u32,
    known_human_accepted: u32,
    ai_accepted_by_tool: &BTreeMap<String, u32>,
) -> CommitStats {
    let mut commit_stats = CommitStats {
        human_additions: 0,
        unknown_additions: 0,
        ai_additions: 0,
        ai_accepted,
        time_waiting_for_ai: 0,
        tool_model_breakdown: BTreeMap::new(),
        git_diff_deleted_lines,
        git_diff_added_lines,
    };

    if let Some(log) = authorship_log {
        // Process old-format prompts (only extract waiting time)
        for prompt_record in log.metadata.prompts.values() {
            let key = format!(
                "{}::{}",
                prompt_record.agent_id.tool, prompt_record.agent_id.model
            );
            let tool_stats = commit_stats.tool_model_breakdown.entry(key).or_default();

            let transcript = crate::authorship::transcript::AiTranscript {
                messages: prompt_record.messages.clone(),
            };
            let waiting = calculate_waiting_time(&transcript);
            commit_stats.time_waiting_for_ai += waiting;
            tool_stats.time_waiting_for_ai += waiting;
        }

        // Process new-format sessions (only extract waiting time)
        for session_record in log.metadata.sessions.values() {
            let key = format!(
                "{}::{}",
                session_record.agent_id.tool, session_record.agent_id.model
            );
            let tool_stats = commit_stats.tool_model_breakdown.entry(key).or_default();

            let transcript = crate::authorship::transcript::AiTranscript {
                messages: session_record.messages.clone(),
            };
            let waiting = calculate_waiting_time(&transcript);
            commit_stats.time_waiting_for_ai += waiting;
            tool_stats.time_waiting_for_ai += waiting;
        }
    }

    // Update tool-level accepted counts using diff-based attribution.
    for (tool_model, accepted) in ai_accepted_by_tool {
        let tool_stats = commit_stats
            .tool_model_breakdown
            .entry(tool_model.clone())
            .or_default();
        tool_stats.ai_accepted = *accepted;
    }

    // AI additions now equal ai_accepted (no mixed component)
    commit_stats.ai_additions = commit_stats.ai_accepted;

    for tool_stats in commit_stats.tool_model_breakdown.values_mut() {
        tool_stats.ai_additions = tool_stats.ai_accepted;
    }

    // KnownHuman-attested additions
    commit_stats.human_additions = known_human_accepted;

    // Unknown additions: lines with no attestation at all
    commit_stats.unknown_additions = git_diff_added_lines
        .saturating_sub(commit_stats.ai_accepted)
        .saturating_sub(known_human_accepted);

    commit_stats
}
```

- [ ] **Step 2: Verify the function compiles in isolation**

Run: `task build 2>&1 | grep "stats.rs" | head -20`
Expected: stats.rs itself should compile cleanly; remaining errors are in other files.

---

### Task 3: Update Display Functions

**Files:**
- Modify: `src/authorship/stats.rs:97-436` (terminal and markdown display functions)

- [ ] **Step 1: Update `write_stats_to_markdown` — remove mixed row and generated lines ratio**

The markdown function no longer shows the "mixed" bar row or "lines generated for every 1 accepted" detail. Update to:

```rust
#[allow(dead_code)]
pub fn write_stats_to_markdown(stats: &CommitStats) -> String {
    let mut output = String::new();

    let bar_width: usize = 20;

    if stats.git_diff_added_lines == 0 && stats.git_diff_deleted_lines > 0 {
        output.push_str("(no additions)");
        output.push('\n');
        return output;
    }

    let total_additions = stats.git_diff_added_lines;

    let pure_human = stats.human_additions + stats.unknown_additions;
    let pure_ai = stats.ai_accepted;

    let pure_human_percentage = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };
    let ai_percentage = if total_additions > 0 {
        ((pure_ai as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    let pure_human_bars = if total_additions > 0 {
        let calculated =
            ((pure_human as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        if pure_human > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    let ai_bars = if total_additions > 0 {
        let calculated =
            ((pure_ai as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        if pure_ai > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    output.push_str("Stats powered by [Git AI](https://github.com/git-ai-project/git-ai)\n\n");
    output.push_str("```text\n");

    output.push_str("🧠 you    ");
    output.push_str(&"█".repeat(pure_human_bars));
    output.push_str(&"░".repeat(bar_width.saturating_sub(pure_human_bars)));
    output.push_str(&format!("  {}%\n", pure_human_percentage));

    output.push_str("🤖 ai     ");
    output.push_str(&"░".repeat(bar_width.saturating_sub(ai_bars)));
    output.push_str(&"█".repeat(ai_bars));
    output.push_str(&format!("  {}%\n", ai_percentage));

    output.push_str("```");

    // Add details section
    output.push_str("\n\n<details>\n");
    output.push_str("<summary>More stats</summary>\n\n");

    let minutes = stats.time_waiting_for_ai / 60;
    let seconds = stats.time_waiting_for_ai % 60;
    let time_str = if minutes > 0 {
        format!("{} minute{}", minutes, if minutes == 1 { "" } else { "s" })
    } else {
        format!("{} second{}", seconds, if seconds == 1 { "" } else { "s" })
    };
    output.push_str(&format!("- {} waiting for AI \n", time_str));

    if !stats.tool_model_breakdown.is_empty()
        && let Some((model_name, model_stats)) = stats
            .tool_model_breakdown
            .iter()
            .max_by_key(|(_, stats)| stats.ai_accepted)
    {
        output.push_str(&format!(
            "- Top model: {} ({} accepted lines)\n",
            model_name, model_stats.ai_accepted
        ));
    }

    output.push_str("\n</details>");

    output
}
```

- [ ] **Step 2: Update `write_stats_to_terminal` — remove mixed percentage from AI acceptance line**

In the terminal display function (~lines 260-284), the line showing AI acceptance stats references `mixed_additions`. Remove any mention of mixed from the acceptance line. The percentage calculation becomes:

```rust
// Acceptance line: just show percentage and wait time
let acceptance_pct = if stats.ai_additions > 0 {
    ((stats.ai_accepted as f64 / stats.ai_additions as f64) * 100.0).round() as u32
} else {
    0
};
```

Since `ai_additions == ai_accepted` now (no mixed component), the acceptance line can be simplified or removed. Just show the wait time if > 0:

Find the line that formats acceptance and replace it to only show wait time when > 0.

- [ ] **Step 3: Verify build**

Run: `task build 2>&1 | grep "stats.rs" | head -20`
Expected: No errors in stats.rs

---

### Task 4: Update Telemetry in `post_commit.rs`

**Files:**
- Modify: `src/authorship/post_commit.rs:740-810`

- [ ] **Step 1: Remove mixed/total_ai_additions/total_ai_deletions from telemetry emission**

Remove lines that reference `mixed_additions`, `total_ai_additions`, `total_ai_deletions` from the `emit_committed_event` function. The parallel arrays for these fields should be removed.

Replace lines 754-806 with:

```rust
    let mut agg_ai = stats.ai_additions;
    let mut agg_accepted = stats.ai_accepted;
    let mut agg_waiting: u64 = stats.time_waiting_for_ai;
    for (key, ts) in &stats.tool_model_breakdown {
        if key.starts_with("mock_ai::") {
            agg_ai = agg_ai.saturating_sub(ts.ai_additions);
            agg_accepted = agg_accepted.saturating_sub(ts.ai_accepted);
            agg_waiting = agg_waiting.saturating_sub(ts.time_waiting_for_ai);
        }
    }

    let mut tool_model_pairs: Vec<String> = vec!["all".to_string()];
    let mut ai_additions: Vec<u32> = vec![agg_ai];
    let mut ai_accepted: Vec<u32> = vec![agg_accepted];
    let mut time_waiting_for_ai: Vec<u64> = vec![agg_waiting];

    for (tool_model, tool_stats) in &stats.tool_model_breakdown {
        if tool_model.starts_with("mock_ai::") {
            continue;
        }
        tool_model_pairs.push(tool_model.clone());
        ai_additions.push(tool_stats.ai_additions);
        ai_accepted.push(tool_stats.ai_accepted);
        time_waiting_for_ai.push(tool_stats.time_waiting_for_ai);
    }

    let values = CommittedValues::new()
        .human_additions(stats.human_additions)
        .git_diff_deleted_lines(stats.git_diff_deleted_lines)
        .git_diff_added_lines(stats.git_diff_added_lines)
        .tool_model_pairs(tool_model_pairs)
        .ai_additions(ai_additions)
        .ai_accepted(ai_accepted)
        .time_waiting_for_ai(time_waiting_for_ai);
```

- [ ] **Step 2: Remove `mixed_additions`, `total_ai_additions`, `total_ai_deletions` from `CommittedValues` builder**

In `src/metrics/events.rs`, remove or mark deprecated:
- `fn mixed_additions(...)` and `fn mixed_additions_null(...)`
- `fn total_ai_additions(...)` and `fn total_ai_additions_null(...)`
- `fn total_ai_deletions(...)` and `fn total_ai_deletions_null(...)`

If other code still references these builder methods, remove those references. If the builder methods are truly unused after the post_commit change, delete them.

- [ ] **Step 3: Verify build**

Run: `task build 2>&1 | head -40`
Expected: Compilation succeeds or only test-related errors remain.

---

### Task 5: Update Unit Tests in `stats.rs`

**Files:**
- Modify: `src/authorship/stats.rs:780-1940` (test module)

- [ ] **Step 1: Remove `mixed_additions`, `total_ai_additions`, `total_ai_deletions` from all `CommitStats` literals in tests**

Every test that constructs a `CommitStats` directly (the snapshot display tests) needs those three fields removed. There are ~14 instances in `test_terminal_stats_display` and ~5 in `test_markdown_stats_display`.

For each `CommitStats { ... }` literal, remove lines:
- `mixed_additions: <value>,`
- `total_ai_additions: <value>,`
- `total_ai_deletions: <value>,`

Since those fields no longer exist on the struct, the code won't compile until they're removed.

- [ ] **Step 2: Remove or update `test_stats_from_authorship_log_mixed_cap`**

This test specifically tests capping of `mixed_additions` from `PromptRecord.overriden_lines`. Since mixed is gone, delete this entire test (it's already `#[ignore]`).

- [ ] **Step 3: Update `test_stats_from_authorship_log_no_log`**

Remove assertions for `mixed_additions`, `total_ai_additions`, `total_ai_deletions`:

```rust
#[test]
fn test_stats_from_authorship_log_no_log() {
    let stats = stats_from_authorship_log(None, 10, 5, 3, 0, &BTreeMap::new());

    assert_eq!(stats.git_diff_added_lines, 10);
    assert_eq!(stats.git_diff_deleted_lines, 5);
    assert_eq!(stats.ai_accepted, 3);
    assert_eq!(stats.ai_additions, 3);
    assert_eq!(stats.human_additions, 0);
    assert_eq!(stats.unknown_additions, 7);
    assert_eq!(stats.time_waiting_for_ai, 0);
}
```

- [ ] **Step 4: Update `test_stats_for_merge_commit_skips_ai_acceptance`**

Remove the assertion `assert_eq!(stats.ai_additions, stats.mixed_additions);` since `mixed_additions` no longer exists. Replace with:

```rust
assert_eq!(stats.ai_additions, 0); // merge commits have 0 ai_accepted
```

- [ ] **Step 5: Verify unit tests compile**

Run: `task build 2>&1 | head -40`
Expected: Clean compilation

---

### Task 6: Update Integration Tests

**Files:**
- Modify: `tests/integration/stats.rs:84-209`

- [ ] **Step 1: Remove mixed/total_ai assertions from `test_authorship_log_stats`**

Remove these assertion lines (or update them):
- Line 148: `assert_eq!(stats.mixed_additions, 0);` — remove (field gone)
- Lines 151-153: `total_ai_additions` / `total_ai_deletions` assertions — remove
- Lines 175-190: tool breakdown `total_ai_additions`, `total_ai_deletions`, `mixed_additions` assertions — remove

The test should become:

```rust
assert_eq!(stats.human_additions, 4);
assert_eq!(stats.unknown_additions, 0);
assert_eq!(stats.ai_additions, 5);
assert_eq!(stats.ai_accepted, 5);
assert_eq!(stats.git_diff_deleted_lines, 0);
assert_eq!(stats.git_diff_added_lines, 9);

assert_eq!(stats.tool_model_breakdown.len(), 1);
assert_eq!(
    stats.tool_model_breakdown.get("mock_ai::unknown").unwrap().ai_additions,
    5
);
assert_eq!(
    stats.tool_model_breakdown.get("mock_ai::unknown").unwrap().ai_accepted,
    5
);
assert_eq!(
    stats.tool_model_breakdown.get("mock_ai::unknown").unwrap().time_waiting_for_ai,
    0
);
```

- [ ] **Step 2: Search for other integration test files that reference removed fields**

Run: `grep -rn "mixed_additions\|total_ai_additions\|total_ai_deletions" tests/`

Fix any remaining references in other integration test files.

- [ ] **Step 3: Verify integration tests compile**

Run: `task build 2>&1 | head -40`
Expected: Clean compilation

---

### Task 7: Update Snapshot Tests

**Files:**
- Modify: Various `.snap` files in `src/authorship/snapshots/` and `tests/integration/snapshots/`

- [ ] **Step 1: Run all tests and accept snapshot updates**

Run: `task test 2>&1 | tail -30`

Many snapshot tests will fail because the output format has changed (no "mixed" row in markdown, no "lines generated" in details, terminal output simplified).

- [ ] **Step 2: Review and accept snapshot changes**

Run: `cargo insta review`

Accept all pending snapshots. The changes should be:
- Markdown snapshots: no "🤝 mixed" row, no "X.X lines generated for every 1 accepted", "Top model" line now shows only accepted (not generated)
- Terminal snapshots: acceptance line simplified
- Integration snapshots (worktrees, etc.): `CommitStats` debug output won't have removed fields

- [ ] **Step 3: Run tests again to confirm all pass**

Run: `task test`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: remove legacy prompt stats fields from CommitStats

Drop mixed_additions, total_ai_additions, total_ai_deletions from
CommitStats and ToolModelHeadlineStats. Sessions don't track these
metrics — stats are now derived purely from diff-based attribution.
Update display, telemetry, and all tests accordingly."
```

---

### Task 8: Clean Up `SessionRecord::to_prompt_record` (if no longer needed)

**Files:**
- Modify: `src/authorship/authorship_log.rs:232-246`

- [ ] **Step 1: Check if `to_prompt_record` is still used**

Run: `grep -rn "to_prompt_record" src/ tests/`

The `to_prompt_record` on `SessionRecord` fills stats fields with 0. If it's only used in places that no longer need the stats fields, check whether those call sites can use `SessionRecord` directly instead.

Note: `to_prompt_record` is also on the internal DB record type and is used in search/share/continue_session commands. These non-stats usages may still need PromptRecord for the transcript/messages. This step is about removing the `SessionRecord::to_prompt_record` specifically if nothing consumes it after the stats cleanup.

- [ ] **Step 2: If unused, remove `SessionRecord::to_prompt_record`**

Delete the method from `SessionRecord` impl block.

- [ ] **Step 3: Verify build**

Run: `task build`
Expected: Clean compilation

- [ ] **Step 4: Run full test suite**

Run: `task test`
Expected: All pass

- [ ] **Step 5: Commit (if changes were made)**

```bash
git add src/authorship/authorship_log.rs
git commit -m "refactor: remove unused SessionRecord::to_prompt_record"
```
