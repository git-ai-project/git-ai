# git-ai Performance Regression Deep Dive (`feat/corehooks` vs `main`)

Date: 2026-02-15  
Branch under test: `feat/corehooks` (`e5b377a1`)  
Baseline: `main` (`3ce057c7`)

## Executive summary

1. The plain wrapper path is consistently slower on `feat/corehooks`, primarily due to extra repository discovery work added in commit `506bafdd` (`Support bare repo discovery in find_repository`).
2. The core-hooks path is much slower than wrapper mode because commit operations fan out into many hook invocations, and each hook invocation runs expensive startup + repository checks before doing hook logic.
3. On tested workloads:
   - Wrapper mode regression vs `main`: typically ~10-15% on fast commands.
   - Core-hooks mode vs wrapper mode (`feat/corehooks`): ~4.3x on a realistic multi-commit workload.
   - Core-hooks mode vs plain git (tiny commits): can exceed 30x, because fixed hook overhead dominates tiny commits.

## What changed and why it matters

## 1) Wrapper regression root cause (commit `506bafdd`)

### Relevant code
- `/Users/svarlamov/projects/git-ai-c/src/git/repository.rs:1928`
- `/Users/svarlamov/projects/git-ai-c/src/git/repository.rs:1934`
- `/Users/svarlamov/projects/git-ai-c/src/git/repository.rs:1985`

`find_repository()` was refactored to support bare repos. For non-bare repos it now does:

1. `git rev-parse --is-bare-repository --git-dir`
2. `git rev-parse --show-toplevel`

In `main`, this was effectively one `rev-parse` call for both values.

That adds one extra git subprocess to nearly every wrapped command. On fast commands, this fixed cost is large enough to create visible regression.

### Evidence
- `git blame` attributes these lines to `506bafdd`.
- Trace comparison (`git rev-parse --is-inside-work-tree` through wrapper):
  - `main`: 5 git `start` events
  - `feat/corehooks`: 6 git `start` events
  - The extra event is the additional `rev-parse --show-toplevel` leg.

## 2) Core-hooks slowdown root causes

### A) Hook fan-out (many hooks execute per commit)

Relevant code:
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:29`
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:44`
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:45`

`INSTALLED_HOOKS` includes 16 hooks, including passthrough hooks for compatibility. A single commit can trigger many of them (not just pre/post-commit), especially multiple `reference-transaction` stages.

### B) Core-hooks are installed/configured by default in `install-hooks`

Relevant code:
- `/Users/svarlamov/projects/git-ai-c/src/commands/install_hooks.rs:189`
- `/Users/svarlamov/projects/git-ai-c/src/commands/install_hooks.rs:193`
- `/Users/svarlamov/projects/git-ai-c/src/commands/install_hooks.rs:696`

`git-ai install-hooks` now configures `core.hooksPath` and writes the managed hook scripts by default, so users adopting install flow will run through this heavier path.

### C) Each hook invocation is a new process + duplicated repository discovery

Relevant code:
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:1509`
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:1532`
- `/Users/svarlamov/projects/git-ai-c/src/commands/git_ai_handlers.rs:31`
- `/Users/svarlamov/projects/git-ai-c/src/commands/git_ai_handlers.rs:36`
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:126`

Flow per hook:
1. Hook shim executes `git-ai hook <hook>` (new process).
2. `handle_git_ai()` preamble runs repo lookup + allowlist check.
3. `handle_core_hook_command()` then calls `find_repository_for_hook()` again.

So repository discovery happens twice per hook invocation.

### D) Allowlist check always fetches remotes

Relevant code:
- `/Users/svarlamov/projects/git-ai-c/src/config.rs:185`
- `/Users/svarlamov/projects/git-ai-c/src/config.rs:187`
- `/Users/svarlamov/projects/git-ai-c/src/git/repository.rs:1026`

`Config::is_allowed_repository()` always asks for remotes (`remote -v`) before checking whether allow/exclude lists are empty. In hook mode, this cost is paid on every hook process.

### E) Some hook handlers run substantial git work

Relevant code:
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:200`
- `/Users/svarlamov/projects/git-ai-c/src/commands/core_hooks.rs:170`

`post-commit` and `reference-transaction` handlers invoke rewrite/authorship logic and additional git queries, increasing cost beyond startup overhead.

## Measurement results

## A) Wrapper mode on this repo (`/Users/svarlamov/projects/git-ai-c`)

5 rounds, looped commands:

- `rev-parse` (200 iterations):
  - `main`: 4.53-4.69s
  - `feat/corehooks`: 5.07-5.26s
  - Regression: ~11-16%

- `status --porcelain` (150 iterations):
  - `main`: 3.81-4.10s
  - `feat/corehooks`: 4.33-4.43s
  - Regression: ~6-16%

Additional controlled run (temp repo, 500 rev-parse iterations) averaged:
- `main`: 11.20s
- `feat/corehooks`: 12.78s
- Regression: +14.1%

## B) Core-hooks mode vs plain git (same `feat/corehooks` binary)

Benchmark (temp repos):

- `status` (300 iterations):  
  plain `2290.658ms` vs core-hooks `2543.898ms` (~1.11x)

- `add+reset` loop (200 iterations):  
  plain `3326.980ms` vs core-hooks `27430.257ms` (~8.2x)

- `commit` loop (20 commits):  
  plain `583.819ms` vs core-hooks `24014.044ms` (~41.1x)

Realistic multi-commit workload (core-hooks vs wrapper, same branch):
- wrapper `1362.861ms`
- core-hooks `5849.017ms`
- ratio `4.29x`

## C) Process fan-out evidence

Trace for one `add+commit`:
- plain git: `3` start events
- core-hooks: `94` start events

Hook child events included:
- `post-index-change` x2
- `pre-commit` x1
- `prepare-commit-msg` x1
- `commit-msg` x1
- `reference-transaction` x7
- `post-commit` x1

This matches the observed multiplicative slowdown.

## Commits most responsible

1. `506bafdd` - bare-repo support refactor in `find_repository` (plain wrapper regression).
2. `7ca04dd3` - initial core-hooks support:
   - adds `hook` command path
   - adds core-hooks install/configuration in install flow
   - adds initial hook script/handler framework.
3. `172979c6` (and follow-up core-hooks hardening commits) - expands/solidifies hook compatibility set, increasing hook-trigger surface.

## Notes

- `GIT_AI_SKIP_CORE_HOOKS_ENV` plumbing in wrapper proxy path (`/Users/svarlamov/projects/git-ai-c/src/commands/git_handlers.rs:508`) is not a major contributor by itself.
- Absolute slowdown magnitude varies by workload size; the fixed overhead is worst on short commands and small commits.
