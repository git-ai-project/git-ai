# git-ai 

<img src="https://github.com/git-ai-project/git-ai/raw/main/assets/docs/git-ai.png" align="right"
     alt="Git AI Logo" width="200" height="200">

Git AI is an open source git extension that tracks the AI-generated code in your repositories. 

Once installed, every AI line is automatically linked to the agent, model, and prompts that generated it — ensuring the intent, requirements, and architecture decisions behind your code are never forgotten.

**AI attribution linked to every commit:**

`git commit` 
```
[hooks-doctor 0afe44b2] wsl compat check
 2 files changed, 81 insertions(+), 3 deletions(-)
you  ██░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░ ai
     6%             mixed   2%             92%
```

**AI Blame tracks model, agent and session behind every line:**

`git-ai blame /src/log_fmt/authorship_log.rs`
```bash

cb832b7 (Aidan Cunniffe                2025-12-13 08:16:29 -0500  133) pub fn execute_diff(
cb832b7 (Aidan Cunniffe                2025-12-13 08:16:29 -0500  134)     repo: &Repository,
cb832b7 (Aidan Cunniffe                2025-12-13 08:16:29 -0500  135)     spec: DiffSpec,
cb832b7 (Aidan Cunniffe                2025-12-13 08:16:29 -0500  136)     format: DiffFormat,
cb832b7 (Aidan Cunniffe                2025-12-13 08:16:29 -0500  137) ) -> Result<String, GitAiError> {
fe2c4c8 (claude-4.5-opus [session_id]  2025-12-02 19:25:13 -0500  138)     // Resolve commits to get from/to SHAs
fe2c4c8 (claude-4.5-opus [session_id]  2025-12-02 19:25:13 -0500  139)     let (from_commit, to_commit) = match spec {
fe2c4c8 (claude-4.5-opus [session_id]  2025-12-02 19:25:13 -0500  140)         DiffSpec::TwoCommit(start, end) => {
fe2c4c8 (claude-4.5-opus [session_id]  2025-12-02 19:25:13 -0500  141)             // Resolve both commits
fe2c4c8 (claude-4.5-opus [session_id]  2025-12-02 19:25:13 -0500  142)             let from = resolve_commit(repo, &start)?;...
```

### Supported Agents:

> <img src="assets/docs/badges/claude_code.svg" alt="Claude Code" height="25" /> <img src="assets/docs/badges/codex-black.svg" alt="Codex" height="25" /> <img src="assets/docs/badges/cursor.svg" alt="Cursor" height="25" /> <img src="assets/docs/badges/opencode.svg" alt="OpenCode" height="25" /> <img src="assets/docs/badges/gemini.svg" alt="Gemini" height="25" /> <img src="assets/docs/badges/copilot.svg" alt="GitHub Copilot" height="25" /> <img src="assets/docs/badges/continue.svg" alt="Continue" height="25" /> <img src="assets/docs/badges/droid.svg" alt="Droid" height="25" /> <img src="assets/docs/badges/junie_white.svg" alt="Junie" height="25" /> <img src="assets/docs/badges/rovodev.svg" alt="Rovo Dev" height="25" />
>
> [+ Add support for another agent](https://usegitai.com/docs/cli/add-your-agent)


### Our Choices:
- **No workflow changes** — Just prompt and commit. Git AI tracks AI-code accurately without making your git history messy.
- **"Detecting" AI-code is an anti-pattern** — Git AI doesn't guess if a hunk is AI-generated. Supported agents tell Git AI exactly which lines they wrote, giving you the most accurate AI-attribution possible.
- **Local-first** — Works offline, no OpenAI or Anthropic key required.
- **Git Native & Open Standard** — Git AI created the [open standard](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md) for tracking AI-generated code with Git Notes.
- **Prompts stay out of Git** — Git Notes reference prompts and agent sessions, but prompt content is never stored in your repository — keeping repos lean, free of API keys + sensitive information, and giving you access controls over prompt data. 

## Install

Mac, Linux, Windows (WSL)

```bash
curl -sSL https://usegitai.com/install.sh | bash
```

Windows (non-WSL)

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://usegitai.com/install.ps1 | iex"
```

🎊 That's it! **No per-repo setup.**



<details>
<summary>How does Git AI work?</summary>


- Agents tell Git AI what code they wrote via Pre/Post Edit Hooks. 
- Each agent edit is stored as a checkpoint — a small diff stored in `.git/ai/` that records whether the change was AI-generated or human-authored. Checkpoints accumulate as you work.
- On Commit, all checkpoints are processed into an Authorship Log that links line ranges to Agent Sessions. This Authorship Log is attached to the commit via a Git Note.
- Git AI ensures attribution survives rebases, merges, squashes, stash/pops, cherry-picks, amends, etc -- transparently rewriting Authorship Logs whenever history is rewritten. 

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

The format of the notes is outlined in the [Git AI Standard v3.0.0](https://github.com/git-ai-project/git-ai/blob/main/specs/git_ai_standard_v3.0.0.md).

</details>


## AI-Blame 

Git AI blame is a drop-in replacement for `git blame` that reports the AI attribution for each line and is compatible with [all the `git blame` flags](https://git-scm.com/docs/git-blame).

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

In VSCode, Cursor, Windsurf and Antigravity the [Git AI extension](https://marketplace.visualstudio.com/items?itemName=git-ai.git-ai-vscode) shows AI-blame decorations in the gutter. Each agent session is color-coded so you can see which prompts generated each huhnk. If you have prompt storage setup you can hover over the line to see the raw prompt / summary. 

<img width="1192" height="890" alt="image" src="https://github.com/user-attachments/assets/94e332e7-5d96-4e5c-8757-63ac0e2f88e0" />

Also available in:
- Emacs magit - https://github.com/jwiegley/magit-ai
- *...have you built support into another editor? Open a PR and we'll add it here*  

## Understand why with the `/ask` skill

See something you don't understand? The `/ask` skill lets you talk to the agent who wrote the code about its instructions, decisions, and the intent of the engineer who assigned it the task. 

The `/ask` skill is added to `~/.agents/skills/` and `~/.claude/skills/` when you install Git AI allowing you to invoke it Cursor, Claude Code, Copilot, Codex, etc just by typing `/ask`:

```
/ask Why didn't we use the SDK here?
```

Agents with access to the original intent and the source code understand the "why". Agents who can only read the code, can tell you what the code does, but not why: 

| Reading Code + Prompts (`/ask`) | Only Reading Code (not using Git AI) |
|---|---|
| When Aidan was building telemetry, he instructed the agent not to block the exit of our CLI flushing telemetry. Instead of using the Sentry SDK directly, we came up with a pattern that writes events locally first via `append_envelope()`, then flushes them in the background via a detached subprocess. This keeps the hot path fast and ships telemetry async after the fact. | `src/commands/flush_logs.rs` is a 5-line wrapper that delegates to `src/observability/flush.rs` (~700 lines). The `commands/` layer handles CLI dispatch; `observability/` handles Sentry, PostHog, metrics upload, and log processing. Parallel modules like `flush_cas`, `flush_logs`, `flush_metrics_db` follow the same thin-dispatch pattern. |


## Make your agents smarter
Agents make fewer mistakes, and produce more maintainable code, when they understand the requirements and decisions behind the code they're building on. We've found the best way to provide this context is just to provide agents with the same `/ask` tool we built for engineers. Tell your Agents to use `/ask` in Plan mode: 

`Claude|AGENTS.md`
```markdown
- In plan mode, always use the /ask skill so you can read the code and the original prompts that generated it. Intent will help you write a better plan
```

## Cross Agent Observability

Measure the % of AI-generated code, accepted-rate by agent and model, and human override rate — for any commit or range of commits.

```bash
git-ai stats --json
```

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

- **AI code composition** — track what percentage of code is AI-generated across your org
- **Track full lifecycle of AI-code** — how much is accepted? Committed? Rewritten during Code Review? Deployed to production? How durable is that code once it ships? Is it the cause of any alerts / incidents?
- **Team workflows** - who is using background agents effectively? Running agents in parallel? What do teams / projects that are getting the most lift from AI doing differently? 
- **Agent Readiness** — track the effectiveness of Agents in your repos. Measure impact of skills, rules, mcps, `AGENTS.md` changes, across repos and task types.
- **Agent + model comparison** — see accepted-rate and output quality by agent and model
  
**[Get early access](https://calendly.com/acunniffe/meeting-with-git-ai-authors)**

# License 
Apache 2.0
