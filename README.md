# git-ai

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI Logo" width="200" height="200">

Git AI is an open source git extension that tracks AI-generated code in your repositories.

Once installed, it automatically links every AI-written line to the agent, model, and transcripts that generated it — so you never lose the intent, requirements, and architecture decisions behind your code.

**AI attribution on every commit:**

`git commit`
```
[hooks-doctor 0afe44b2] wsl compat check
 2 files changed, 81 insertions(+), 3 deletions(-)
you  ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ ai
     6%             mixed   2%             92%
```

**AI Blame shows the model, agent, and session behind every line:**

`git-ai blame /src/log_fmt/authorship_log.rs`
```bash

cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  133) pub fn execute_diff(
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  134)     repo: &Repository,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  135)     spec: DiffSpec,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  136)     format: DiffFormat,
cb832b7 (Aidan Cunniffe      2025-12-13 08:16:29 -0500  137) ) -> Result<String, GitAiError> {
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  138)     // Resolve commits to get from/to SHAs
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  139)     let (from_commit, to_commit) = match spec {
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  140)         DiffSpec::TwoCommit(start, end) => {
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  141)             // Resolve both commits
fe2c4c8 (claude [session_id] 2025-12-02 19:25:13 -0500  142)             let from = resolve_commit(repo, &start)?;...
```


### Supported Agents

<img src="assets/docs/badges/claude_code.svg" alt="Claude Code" height="30" />  <img src="assets/docs/badges/codex-black.svg" alt="Codex" height="30" />  <img src="assets/docs/badges/cursor.svg" alt="Cursor" height="30" />  <img src="assets/docs/badges/opencode.svg" alt="OpenCode" height="30" />  <img src="assets/docs/badges/gemini.svg" alt="Gemini" height="30" />  <img src="assets/docs/badges/copilot.svg" alt="GitHub Copilot" height="30" />  <img src="assets/docs/badges/continue.svg" alt="Continue" height="30" />  <img src="assets/docs/badges/droid.svg" alt="Droid" height="30" />  <img src="assets/docs/badges/junie_white.svg" alt="Junie" height="30" />  <img src="assets/docs/badges/rovodev.svg" alt="Rovo Dev" height="30" />

> [+ Add support for another agent](https://usegitai.com/docs/cli/add-your-agent)


## Install

Mac, Linux, Windows (WSL)

```bash
curl -sSL https://usegitai.com/install.sh | bash
```

Windows (non-WSL)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://usegitai.com/install.ps1 | iex"
```

That's it — **no per-repo setup required.** Prompt and commit as normal. Git AI tracks attribution automatically.


## Our Choices
- **No workflow changes** — Just prompt and commit. Git AI tracks AI code accurately without cluttering your git history.
- **"Detecting" AI code is an anti-pattern** — Git AI does not guess whether a hunk is AI-generated. Supported agents report exactly which lines they wrote, giving you the most accurate attribution possible.
- **Local-first** — Works 100% offline, no login required.
- **Git native and open standard** — Git AI uses an [open standard](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md) for tracking AI-generated code with Git Notes.
- **Transcripts stay out of Git** — Git Notes link to transcripts stored locally, in the Git AI Cloud, or in a self-hosted prompt store -- keeping your repos lean, free of sensitive information, and giving you control over your data.


<table style="table-layout:fixed; width:100%">
<tr>
<th align="center" width="50%">Solo</th>
<th align="center" width="50%">For Teams</th>
</tr>
<tr>
<td align="center"><img src="https://github.com/git-ai-project/git-ai/raw/new-readme/assets/docs/solo-player.svg" alt="Solo — everything stays on your machine" width="400"></td>
<td align="center"><img src="https://github.com/git-ai-project/git-ai/raw/new-readme/assets/docs/for-teams.svg" alt="For teams — shared context across your team" width="400"></td>
</tr>
<tr>
<td valign="top">

- AI Authorship stored in Git Notes, with pointers to transcripts stored in local SQLite
- Transcripts only stored locally, on computer
- Restart any transcript
- Measure AI authorship across commits with `git-ai stats`

</td>
<td valign="top">

- AI Authorship stored in Git Notes, with pointers to cloud or self-hosted transcript store with built-in access control, secret redaction, and PII filtering
- Agents and engineers can read transcripts and summaries for any block of AI-generated code
- Restart any transcript, by any contributor
- Advanced cross-agent dashboards to measure AI adoption, code durability, and compare agents across your team 

**[Click here to get early access](https://calendly.com/acunniffe/meeting-with-git-ai-authors)**

</td>
</tr>
</table>

# Understand Why with the `/ask` Skill

See something you don't understand? The `/ask` skill lets you talk to the agent that wrote the code about its instructions, decisions, and the intent of the engineer who assigned the task.

Git AI adds the `/ask` skill to `~/.agents/skills/` and `~/.claude/skills/` at install time, so you can invoke it from Cursor, Claude Code, Copilot, Codex, and others just by typing `/ask`:

```
/ask Why didn't we use the SDK here?
```

Agents with access to the original intent and source code understand the "why." Agents that can only read the code can tell you what it does, but not why:

| Reading Code + Transcript (`/ask`) | Only Reading Code (not using Git AI) |
|---|---|
| When Aidan was building telemetry, he instructed the agent not to block the exit of our CLI flushing telemetry. Instead of using the Sentry SDK directly, we came up with a pattern that writes events locally first via `append_envelope()`, then flushes them in the background via a detached subprocess. This keeps the hot path fast and ships telemetry async after the fact. | `src/commands/flush_logs.rs` is a 5-line wrapper that delegates to `src/observability/flush.rs` (~700 lines). The `commands/` layer handles CLI dispatch; `observability/` handles Sentry, PostHog, metrics upload, and log processing. Parallel modules like `flush_cas`, `flush_logs`, `flush_metrics_db` follow the same thin-dispatch pattern. |


# Make Your Agents Smarter
Agents make fewer mistakes and produce more maintainable code when they understand the requirements and decisions behind the code they build on. The best way to provide this context is to give agents the same `/ask` tool you use yourself. Tell your agents to use `/ask` in plan mode:

`Claude|AGENTS.md`
```markdown
- In plan mode, always use the /ask skill to read the code and the original transcript that generated it. Understanding intent will help you write a better plan.
```



# AI Blame

Git AI blame is a drop-in replacement for `git blame` that shows AI attribution for each line. It supports [all standard `git blame` flags](https://git-scm.com/docs/git-blame).

```bash
git-ai blame /src/log_fmt/authorship_log.rs
```

```bash
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  133) pub fn execute_diff(
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  134)     repo: &Repository,
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  135)     spec: DiffSpec,
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  136)     format: DiffFormat,
cb832b7 (Aidan Cunniffe 2025-12-13 08:16:29 -0500  137) ) -> Result<String, GitAiError> {
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  138)     // Resolve commits to get from/to SHAs
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  139)     let (from_commit, to_commit) = match spec {
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  140)         DiffSpec::TwoCommit(start, end) => {
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  141)             // Resolve both commits
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  142)             let from = resolve_commit(repo, &start)?;
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  143)             let to = resolve_commit(repo, &end)?;
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  144)             (from, to)
fe2c4c8 (claude         2025-12-02 19:25:13 -0500  145)         }
```

### IDE Plugins

AI blame decorations in the gutter, color-coded by agent session. Hover over a line to see the raw prompt or summary.

<table style="table-layout:fixed; width:100%">
<tr>
<th width="35%">Supported Editors</th>
<th width="65%"></th>
</tr>
<tr>
<td valign="top">

- [VS Code](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Cursor](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Windsurf](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Antigravity](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode)
- [Emacs magit](https://github.com/jwiegley/magit-ai)
- *Built support for another editor? [Open a PR](https://github.com/git-ai-project/git-ai/pulls)*

</td>
<td>
<img width="100%" alt="Git AI VS Code extension showing color-coded AI blame in the gutter" src="https://github.com/user-attachments/assets/94e332e7-5d96-4e5c-8757-63ac0e2f88e0" />
</td>
</tr>
</table>

# Cross Agent Observability

Git AI collects cross-agent telemetry from prompt to production. Track how much AI code gets accepted, committed, through code review, and into production — so you can identify which tools and practices work best for your team.

```bash
git-ai stats --json
```

Learn more: [Stats command reference docs](https://usegitai.com/docs/cli/reference#stats)

```json
{
  "human_additions": 28,
  "mixed_additions": 5,
  "ai_additions": 76,
  "ai_accepted": 47,
  "total_ai_additions": 120,
  "total_ai_deletions": 34,
  "time_waiting_for_ai": 240,
  "tool_model_breakdown": {
    "claude_code/claude-sonnet-4-5-20250929": {
      "ai_additions": 76,
      "mixed_additions": 5,
      "ai_accepted": 47,
      "total_ai_additions": 120,
      "total_ai_deletions": 34,
      "time_waiting_for_ai": 240
    }
  }
}
```

For team-wide visibility, [Git AI Enterprise](https://usegitai.com/enterprise) aggregates data at the PR, repository, and organization level:

- **AI code composition** — Track what percentage of code is AI-generated across your org.
- **Full lifecycle tracking** — See how much AI code is accepted, committed, rewritten during code review, and deployed to production. Measure how durable that code is once it ships and whether it causes alerts or incidents.
- **Team workflows** — Identify who uses background agents effectively, who runs agents in parallel, and what teams getting the most lift from AI do differently.
- **Agent readiness** — Measure the effectiveness of agents in your repos. Track the impact of skills, rules, MCPs, and `AGENTS.md` changes across repos and task types.
- **Agent and model comparison** — Compare acceptance rates and output quality by agent and model.

**[Get early access](https://calendly.com/acunniffe/meeting-with-git-ai-authors)**

![Git AI Enterprise dashboard showing AI code metrics across repositories](https://github.com/git-ai-project/git-ai/raw/main/assets/docs/dashboard.png)

<details>
<summary>How does Git AI work?</summary>


- Agents report what code they wrote via pre/post edit hooks.
- Git AI stores each edit as a checkpoint — a small diff in `.git/ai/` that records whether the change is AI-generated or human-authored. Checkpoints accumulate as you work.
- On commit, Git AI processes all checkpoints into an Authorship Log that links line ranges to agent sessions, then attaches the log to the commit via a Git Note.
- Git AI preserves attribution across rebases, merges, squashes, stash/pops, cherry-picks, and amends by transparently rewriting Authorship Logs whenever history changes.

<table>
<tr>
<td><b>Git Note</b> <code>refs/notes/ai #&lt;commitsha&gt;</code></td>
<td><b>`hooks/post_clone_hook.rs`</b></td>
</tr>
<tr>
<td>

```
hooks/post_clone_hook.rs
  a1b2c3d4e5f6a7b8 6-8
  c9d0e1f2a3b4c5d6 16,21,25
---
{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "0.1.4",
  "base_commit_sha": "f4a8b2c...",
  "prompts": {
    "a1b2c3d4e5f6a7b8": {
      "agent_id": {
        "tool": "copilot",
        "model": "codex-5.2"
      },
      "human_author": "Alice Person <alice@example.com>",
      "messages": [],
      "total_additions": 8,
      "total_deletions": 0,
      "accepted_lines": 3,
      "overriden_lines": 0,
      "messages_url": "https://your-prompt-store.dev/cas/a1b2c3d4..."
    },
    "c9d0e1f2a3b4c5d6": {
      "agent_id": {
        "tool": "cursor",
        "model": "sonnet-4.5"
      },
      "human_author": "Jeff Coder <jeff@example.com>",
      "messages": [],
      "total_additions": 5,
      "total_deletions": 2,
      "accepted_lines": 3,
      "overriden_lines": 0,
      "messages_url": "https://your-prompt-store.dev/cas/c9d0e1f2..."
    }
  }
}
```

</td>
<td>

```rust
 1  pub fn post_clone_hook(
 2      parsed_args: &ParsedGitInvocation,
 3      exit_status: std::process::ExitStatus,
 4  ) -> Option<()> {
 5
 6      if !exit_status.success() {
 7          return None;
 8      }
 9
10      let target_dir =
11          extract_clone_target_directory(&parsed_args.command_args)?;
12
13      let repository =
14          find_repository_in_path(&target_dir).ok()?;
15
16      print!("Fetching authorship notes from origin");
17
18      match fetch_authorship_notes(&repository, "origin") {
19          Ok(()) => {
20              debug_log("successfully fetched");
21              print!(", done.\n");
22          }
23          Err(e) => {
24              debug_log(&format!("fetch failed: {}", e));
25              print!(", failed.\n");
26          }
27      }
28
29      Some(())
30  }
```

</td>
</tr>
</table>

The note format is defined in the [Git AI Standard v3.0.0](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md).

</details>

# License
Apache 2.0
