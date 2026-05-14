# Performance Baselines

These are the authoritative performance targets for git-ai. Any change that regresses these numbers requires justification.

Measured on: 2026-05-14
Platform: Linux 6.12.86+deb13-cloud-arm64 (aarch64), Git 2.47.3
Methodology: median of 20 runs, fresh repo, release build

## Budgets

| Operation | Budget | Notes |
|-----------|--------|-------|
| Checkpoint | 3ms | Called on every AI file edit. Zero git spawns. |
| Post-commit (daemon running) | 1ms | Marker file check only, no git spawns. |
| Post-commit (sync fallback) | 3ms | Two git spawns: cat-file + notes add. |
| Blame (100 lines) | 6ms | |
| Blame (500 lines) | 11ms | |
| Blame (1000 lines) | 16ms | |
| Startup (--version) | 1ms | |
| Binary size | 2.4 MB | |
| Clean build | 8s | |

## Why these matter

- **Checkpoint** is the hottest path. Every AI agent fires pre+post checkpoints on every file edit. At 3ms, it's invisible. At 10ms+, users start noticing lag in their AI tools.
- **Post-commit** runs synchronously in the git hook chain. If it's slow, every `git commit` feels slow.
- **Blame** is user-facing. Developers run it interactively and IDEs call it on file open.
- **Binary size** affects install time, CI cache size, and cold start on every invocation.

## How to benchmark

```bash
# Build release
cargo build --release

# Checkpoint (requires a repo with a working log)
cd /tmp && mkdir perf-test && cd perf-test && git init && git commit --allow-empty -m init
echo "line" > f.txt && git add . && git commit -m "base"
echo "new line" >> f.txt
for i in $(seq 1 20); do
  start=$(date +%s%N)
  /path/to/git-ai checkpoint mock_ai f.txt 2>/dev/null
  end=$(date +%s%N)
  echo "$(( (end - start) / 1000000 ))ms"
done

# Post-commit with daemon marker (simulates daemon-already-handled)
HEAD=$(git rev-parse HEAD)
mkdir -p .git/ai/noted && touch .git/ai/noted/$HEAD
for i in $(seq 1 20); do
  start=$(date +%s%N)
  /path/to/git-ai post-commit 2>/dev/null
  end=$(date +%s%N)
  echo "$(( (end - start) / 1000000 ))ms"
done

# Post-commit sync fallback (no marker)
rm -f .git/ai/noted/$HEAD
for i in $(seq 1 20); do
  git notes --ref=ai remove HEAD 2>/dev/null
  start=$(date +%s%N)
  /path/to/git-ai post-commit 2>/dev/null
  end=$(date +%s%N)
  echo "$(( (end - start) / 1000000 ))ms"
  rm -f .git/ai/noted/$HEAD
done

# Blame
git-ai blame --json src/main.rs > /dev/null  # warm
for i in $(seq 1 20); do
  start=$(date +%s%N)
  /path/to/git-ai blame src/main.rs > /dev/null
  end=$(date +%s%N)
  echo "$(( (end - start) / 1000000 ))ms"
done

# Binary size
ls -la target/release/git-ai
```

## Architecture decisions that protect these numbers

1. **Zero git spawns on checkpoint**: repo root and HEAD resolved from filesystem (walk up for .git, read HEAD file, resolve refs through packed-refs).
2. **Daemon marker coordination**: `.git/ai/noted/<sha>` lets the post-commit hook skip with a single `stat()` when the daemon already handled the commit.
3. **Minimal dependencies**: 5 direct deps, 65 total crates. No async runtime, no HTTP client, no SQLite.
4. **Small binary**: 2.4 MB means fast cold start on every invocation — no pagefault penalty from loading unused code.
