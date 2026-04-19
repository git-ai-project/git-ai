- In plan mode, always use the /ask skill so you can read the code and the original prompts that generated it. Intent will help you write a better plan

## Pre-Commit Checks (MANDATORY)

Run all applicable checks **before every commit**. Do not commit if any check fails.

### Rust (`src/`)
- Run `cargo fmt -- --check` — CI enforces this with `-D warnings`. Run `cargo fmt` to auto-fix.

### IntelliJ plugin (`agent-support/intellij/`)

**Run the lint script** on the changed directories:
```bash
bash ~/.claude/projects/-Users-kcbalusu-Desktop-Project-git-ai/scripts/intellij-lint.sh <changed-directory>
```

**Manually verify these patterns** (not all are caught by the lint script):

#### Process I/O — Pipe deadlock prevention
When spawning external processes that use stdin/stdout/stderr:
1. **Never write stdin synchronously before starting stdout/stderr readers.** If content exceeds the OS pipe buffer (~64KB), the parent blocks on stdin write while the child blocks on stdout write — deadlock. Start all stream readers (via `CompletableFuture.supplyAsync`) **before** writing to stdin, and write stdin asynchronously too.
2. **Never read stdout/stderr after `waitFor()`.** Same deadlock from the other direction.
3. Correct flow: `start process → start stdout reader → start stderr reader → write stdin async → waitFor → collect results`.

#### IntelliJ Platform API — Common misuses
1. **`fileOpened` callback: never use `selectedTextEditor`** — it returns the focused editor, not the editor for the `file` parameter. Use `source.getEditors(file).filterIsInstance<TextEditor>()` instead.
2. **Shared state across editors:** debounce timers, alarms, or schedulers must be **per-editor**, not shared. A shared `cancelAllRequests()` or `cancel()` cancels pending work for all editors.
3. **`Disposer.dispose()` for editor state** must be called inside `invokeLater` to avoid threading races with EDT.
4. Verify correct API signatures — missing abstract method overrides, deprecated API usage, wrong method parameter types.

### All code
- Check for compilation errors, API compatibility issues, thread safety, concurrency bugs
- Review for race conditions and shared state issues

## PR Workflow (MANDATORY)

### 1. Always create PRs in draft mode
Use `gh pr create --draft` for every PR. Never create a non-draft PR.

### 2. Thorough code review before marking ready
After creating the draft PR, spin off a sub-agent to do an **extremely thorough review** of the entire changeset:
- Run all pre-commit checks above
- Fix any issues found, amend the commit, and push

**Loop this review-fix cycle until zero issues are found.**

### 3. Monitor CI after push (every 5 minutes)
After pushing to the draft PR, poll CI status every 5 minutes using:
```bash
GH_CONFIG_DIR=/tmp/gh-cfg gh run list --repo git-ai-project/git-ai --branch <branch> --limit 1 --json status,conclusion
```
When a CI run completes:
- If **failed**: fetch the logs, identify the error, fix it, push, and continue monitoring
  ```bash
  GH_CONFIG_DIR=/tmp/gh-cfg gh run view <run-id> --repo git-ai-project/git-ai --log-failed
  ```
- If **passed**: report success to the user. Do NOT mark the PR as ready — the user will do that.
- Distinguish between **our failures** (compilation, verification) and **pre-existing/unrelated failures** (E2E opencode fish_add_path, benchmark margin noise)

**ALL review, fixing, and CI monitoring MUST happen while the PR is in draft mode.**

### 4. GH CLI access
Use `GH_CONFIG_DIR=/tmp/gh-cfg` prefix for all `gh` commands to avoid sandbox config permission issues. The `GH_TOKEN` environment variable provides authentication.

## Task Master AI Instructions
**Import Task Master's development workflow commands and guidelines, treat as if import is in the main CLAUDE.md file.**
@./.taskmaster/CLAUDE.md
