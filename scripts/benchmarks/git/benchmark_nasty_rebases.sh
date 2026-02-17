#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Benchmark git-ai rebase/rewrite performance with large synthetic commit stacks on top of an OSS repo.

Usage:
  benchmark_nasty_rebases.sh [options]

Options:
  --repo-url <url>             OSS repo to clone (default: https://github.com/python/cpython.git)
  --work-root <path>           Working directory (default: /tmp/git-ai-nasty-rebase-<timestamp>)
  --feature-commits <n>        Number of AI feature commits (default: 220)
  --main-commits <n>           Number of upstream main commits (default: 70)
  --side-commits <n>           Number of AI side-branch commits for merge scenario (default: 60)
  --files <n>                  Number of generated feature files (default: 5)
  --lines-per-file <n>         Lines per generated file (default: 3000)
  --burst-every <n>            Every Nth feature commit rewrites all generated files (default: 25)
  --git-bin <path>             Git binary to use (default: wrapper next to git-ai, else PATH git)
  --git-ai-bin <path>          git-ai binary (default: PATH git-ai)
  --hook-mode <mode>           wrapper | hooks | both (default: wrapper)
  --skip-clone                 Reuse existing clone in <work-root>/repo
  -h, --help                   Show help

Outputs:
  - Logs: <work-root>/logs/*.log
  - Summary: <work-root>/summary.txt
EOF
}

REPO_URL="https://github.com/python/cpython.git"
WORK_ROOT=""
FEATURE_COMMITS=220
MAIN_COMMITS=70
SIDE_COMMITS=60
FILES=5
LINES_PER_FILE=3000
BURST_EVERY=25
SKIP_CLONE=0
GIT_BIN=""
GIT_AI_BIN=""
HOOK_MODE="wrapper"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-url) REPO_URL="$2"; shift 2 ;;
    --work-root) WORK_ROOT="$2"; shift 2 ;;
    --feature-commits) FEATURE_COMMITS="$2"; shift 2 ;;
    --main-commits) MAIN_COMMITS="$2"; shift 2 ;;
    --side-commits) SIDE_COMMITS="$2"; shift 2 ;;
    --files) FILES="$2"; shift 2 ;;
    --lines-per-file) LINES_PER_FILE="$2"; shift 2 ;;
    --burst-every) BURST_EVERY="$2"; shift 2 ;;
    --git-bin) GIT_BIN="$2"; shift 2 ;;
    --git-ai-bin) GIT_AI_BIN="$2"; shift 2 ;;
    --hook-mode) HOOK_MODE="$2"; shift 2 ;;
    --skip-clone) SKIP_CLONE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1"; usage; exit 1 ;;
  esac
done

if [[ -z "$GIT_AI_BIN" ]]; then
  GIT_AI_BIN="$(command -v git-ai || true)"
fi

if [[ -z "$GIT_AI_BIN" ]]; then
  echo "error: git-ai not found in PATH" >&2
  exit 1
fi

if [[ -z "$GIT_BIN" ]]; then
  CANDIDATE_WRAP_GIT="$(dirname "$GIT_AI_BIN")/git"
  if [[ -x "$CANDIDATE_WRAP_GIT" ]]; then
    GIT_BIN="$CANDIDATE_WRAP_GIT"
  else
    GIT_BIN="$(command -v git)"
  fi
fi

if [[ -z "$WORK_ROOT" ]]; then
  WORK_ROOT="${TMPDIR:-/tmp}/git-ai-nasty-rebase-$(date +%Y%m%d-%H%M%S)"
fi

if [[ "$HOOK_MODE" != "wrapper" && "$HOOK_MODE" != "hooks" && "$HOOK_MODE" != "both" ]]; then
  echo "error: --hook-mode must be one of wrapper|hooks|both" >&2
  exit 1
fi

REPO_DIR="$WORK_ROOT/repo"
LOG_DIR="$WORK_ROOT/logs"
SUMMARY_FILE="$WORK_ROOT/summary.txt"
RESULTS_TSV="$WORK_ROOT/results.tsv"

mkdir -p "$WORK_ROOT" "$LOG_DIR"

now_ns() {
  python3 - <<'PY'
import time
print(time.time_ns())
PY
}

seconds_from_ns_delta() {
  local start_ns="$1"
  local end_ns="$2"
  python3 - "$start_ns" "$end_ns" <<'PY'
import sys
s = int(sys.argv[1])
e = int(sys.argv[2])
print(f"{(e - s) / 1_000_000_000:.3f}")
PY
}

strip_ansi_file() {
  local src="$1"
  local dst="$2"
  perl -pe 's/\e\[[0-9;]*[A-Za-z]//g' "$src" > "$dst"
}

g() {
  GIT_AI_DEBUG=0 GIT_AI_DEBUG_PERFORMANCE=0 "$GIT_BIN" -C "$REPO_DIR" "$@"
}

generate_file() {
  local path="$1"
  local seed="$2"
  local lines="$3"
  python3 - "$path" "$seed" "$lines" <<'PY'
import os
import sys

path = sys.argv[1]
seed = int(sys.argv[2])
lines = int(sys.argv[3])

os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w", encoding="utf-8") as f:
    for i in range(1, lines + 1):
        payload = (seed * 1315423911 + i * 2654435761) & 0xFFFFFFFF
        f.write(f"seed={seed:08d} line={i:06d} payload={payload:08x}\n")
PY
}

run_ai_checkpoint() {
  (
    cd "$REPO_DIR"
    GIT_AI_DEBUG=0 GIT_AI_DEBUG_PERFORMANCE=0 "$GIT_AI_BIN" checkpoint mock_ai >/dev/null
  )
}

ensure_clean_rebase_state() {
  if [[ -d "$REPO_DIR/.git/rebase-merge" || -d "$REPO_DIR/.git/rebase-apply" ]]; then
    GIT_EDITOR=: g rebase --abort >/dev/null 2>&1 || true
  fi
  g am --abort >/dev/null 2>&1 || true
  g cherry-pick --abort >/dev/null 2>&1 || true
  g merge --abort >/dev/null 2>&1 || true
}

run_rebase_scenario() {
  local scenario="$1"
  local branch="$2"
  shift 2
  local log_file="$LOG_DIR/${scenario}.log"
  local clean_log="$LOG_DIR/${scenario}.clean.log"

  echo
  echo "== Running scenario: $scenario =="
  echo "command: $GIT_BIN -C $REPO_DIR $*"

  ensure_clean_rebase_state
  g checkout "$branch" >/dev/null

  local start_ns
  local end_ns
  local duration_s
  start_ns="$(now_ns)"
  if GIT_AI_DEBUG=1 GIT_AI_DEBUG_PERFORMANCE=1 "$GIT_BIN" -C "$REPO_DIR" "$@" >"$log_file" 2>&1; then
    status="ok"
  else
    status="fail"
  fi
  end_ns="$(now_ns)"
  duration_s="$(seconds_from_ns_delta "$start_ns" "$end_ns")"

  if [[ "$status" == "fail" ]]; then
    ensure_clean_rebase_state
  fi

  strip_ansi_file "$log_file" "$clean_log"

  local mapping_line
  local processing_line
  local saved_count
  local note_state
  local branch_tip

  mapping_line="$(grep -m1 'Commit mapping:' "$clean_log" || true)"
  processing_line="$(grep -m1 'Processing rebase:' "$clean_log" || true)"
  saved_count="$(grep -c 'Saved authorship log for commit' "$clean_log" || true)"

  branch_tip="$(g rev-parse "$branch")"
  if g notes --ref=ai show "$branch_tip" >/dev/null 2>&1; then
    note_state="yes"
  else
    note_state="no"
  fi

  printf "%s\t%s\t%s\t%s\t%s\n" \
    "$scenario" "$status" "$duration_s" "$saved_count" "$note_state" >>"$RESULTS_TSV"

  {
    echo "scenario: $scenario"
    echo "status: $status"
    echo "duration_seconds: $duration_s"
    echo "branch: $branch"
    echo "branch_tip: $branch_tip"
    echo "head_has_ai_note: $note_state"
    echo "saved_authorship_logs: $saved_count"
    echo "mapping: ${mapping_line:-<none>}"
    echo "processing: ${processing_line:-<none>}"
    echo "log: $log_file"
    echo
  } >>"$SUMMARY_FILE"

  echo "status=$status duration=${duration_s}s saved_logs=$saved_count head_note=$note_state"
  if [[ -n "$mapping_line" ]]; then
    echo "mapping: $mapping_line"
  fi
  if [[ -n "$processing_line" ]]; then
    echo "processing: $processing_line"
  fi
}

echo "=== git-ai nasty rebase benchmark ==="
echo "repo_url=$REPO_URL"
echo "work_root=$WORK_ROOT"
echo "repo_dir=$REPO_DIR"
echo "git_bin=$GIT_BIN"
echo "git_ai_bin=$GIT_AI_BIN"
echo "hook_mode=$HOOK_MODE"
echo "feature_commits=$FEATURE_COMMITS main_commits=$MAIN_COMMITS side_commits=$SIDE_COMMITS"
echo "files=$FILES lines_per_file=$LINES_PER_FILE burst_every=$BURST_EVERY"

if [[ "$SKIP_CLONE" -eq 0 ]]; then
  rm -rf "$REPO_DIR"
  echo "Cloning repo..."
  "$GIT_BIN" clone --depth 1 "$REPO_URL" "$REPO_DIR" >/dev/null
fi

if [[ ! -d "$REPO_DIR/.git" ]]; then
  echo "error: repo missing at $REPO_DIR" >&2
  exit 1
fi

DEFAULT_BRANCH="$("$GIT_BIN" -C "$REPO_DIR" rev-parse --abbrev-ref origin/HEAD 2>/dev/null | sed 's|^origin/||')"
if [[ -z "$DEFAULT_BRANCH" || "$DEFAULT_BRANCH" == "HEAD" ]]; then
  if "$GIT_BIN" -C "$REPO_DIR" rev-parse --verify origin/main >/dev/null 2>&1; then
    DEFAULT_BRANCH="main"
  elif "$GIT_BIN" -C "$REPO_DIR" rev-parse --verify origin/master >/dev/null 2>&1; then
    DEFAULT_BRANCH="master"
  else
    DEFAULT_BRANCH="$("$GIT_BIN" -C "$REPO_DIR" rev-parse --abbrev-ref HEAD)"
  fi
fi

echo "default_branch=$DEFAULT_BRANCH"

g config user.name "git-ai bench"
g config user.email "bench@git-ai.local"
g config commit.gpgsign false
g config gc.auto 0

if [[ "$HOOK_MODE" == "hooks" || "$HOOK_MODE" == "both" ]]; then
  echo "Ensuring repo-local git-ai hooks..."
  (
    cd "$REPO_DIR"
    GIT_AI_DEBUG=0 GIT_AI_DEBUG_PERFORMANCE=0 "$GIT_AI_BIN" git-hooks ensure >/dev/null
  )

  HOOKS_PATH="$(g config --local --get core.hooksPath || true)"
  EXPECTED_HOOKS_PATH="$REPO_DIR/.git/ai/hooks"
  python3 - "$HOOKS_PATH" "$EXPECTED_HOOKS_PATH" "$REPO_DIR" <<'PY'
import os
import pathlib
import sys

hooks_path = sys.argv[1].strip()
expected = pathlib.Path(sys.argv[2]).resolve()
repo_dir = pathlib.Path(sys.argv[3]).resolve()
if not hooks_path:
    print("error: expected local core.hooksPath after git-hooks ensure", file=sys.stderr)
    sys.exit(1)

actual = pathlib.Path(hooks_path)
if not actual.is_absolute():
    actual = repo_dir / actual
actual = actual.resolve()
if actual != expected:
    print(
        f"error: expected repo-local hooks path {expected}, got {actual}",
        file=sys.stderr,
    )
    sys.exit(1)

expected_hooks = {
    "pre-commit",
    "prepare-commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "post-rewrite",
    "reference-transaction",
}
installed = {p.name for p in actual.iterdir() if not p.name.startswith(".")}
missing = sorted(expected_hooks - installed)
extras = sorted(installed - expected_hooks)
if missing or extras:
    print(
        "error: unexpected repo-local hook surface "
        f"(missing={missing}, extras={extras})",
        file=sys.stderr,
    )
    sys.exit(1)
PY
fi

g checkout -B bench-main "origin/$DEFAULT_BRANCH" >/dev/null

echo "Seeding large generated files..."
for f in $(seq 1 "$FILES"); do
  generate_file "$REPO_DIR/bench/generated/file_${f}.txt" "$((1000 + f))" "$LINES_PER_FILE"
done
g add -A bench/generated
g commit -m "bench: seed generated files" >/dev/null
BASE_SHA="$(g rev-parse HEAD)"

echo "Creating feature branch with heavy AI commit stack..."
g checkout -B bench-feature "$BASE_SHA" >/dev/null
for i in $(seq 1 "$FEATURE_COMMITS"); do
  if (( i % BURST_EVERY == 0 )); then
    for f in $(seq 1 "$FILES"); do
      generate_file "$REPO_DIR/bench/generated/file_${f}.txt" "$((50000 + i * 1000 + f))" "$LINES_PER_FILE"
    done
  else
    f=$(( (i - 1) % FILES + 1 ))
    generate_file "$REPO_DIR/bench/generated/file_${f}.txt" "$((50000 + i * 1000 + f))" "$LINES_PER_FILE"
  fi

  run_ai_checkpoint
  g add -A bench/generated
  g commit -m "bench(ai): feature commit $i" >/dev/null

  if (( i % 25 == 0 || i == FEATURE_COMMITS )); then
    echo "  feature commits: $i/$FEATURE_COMMITS"
  fi
done
FEATURE_TIP="$(g rev-parse HEAD)"

echo "Creating upstream main churn commits..."
g checkout bench-main >/dev/null
for i in $(seq 1 "$MAIN_COMMITS"); do
  uf=$(( (i - 1) % 3 + 1 ))
  generate_file "$REPO_DIR/bench/upstream/upstream_${uf}.txt" "$((900000 + i))" "$((LINES_PER_FILE / 2))"
  g add -A bench/upstream
  g commit -m "bench(main): upstream commit $i" >/dev/null

  if (( i % 20 == 0 || i == MAIN_COMMITS )); then
    echo "  main commits: $i/$MAIN_COMMITS"
  fi
done
MAIN_TIP="$(g rev-parse HEAD)"

echo "Preparing merge-heavy branch topology..."
FEATURE_CHAIN_FILE="$WORK_ROOT/feature_chain.txt"
g rev-list --reverse "${BASE_SHA}..${FEATURE_TIP}" >"$FEATURE_CHAIN_FILE"
FEATURE_CHAIN_LEN="$(wc -l < "$FEATURE_CHAIN_FILE" | tr -d '[:space:]')"
if [[ "$FEATURE_CHAIN_LEN" -lt 4 ]]; then
  echo "error: not enough feature commits to build merge scenario" >&2
  exit 1
fi
MID_INDEX=$(( FEATURE_CHAIN_LEN / 2 + 1 ))
MID_SHA="$(sed -n "${MID_INDEX}p" "$FEATURE_CHAIN_FILE")"

g checkout -B bench-side "$MID_SHA" >/dev/null
for i in $(seq 1 "$SIDE_COMMITS"); do
  sf=$(( (i - 1) % 3 + 1 ))
  generate_file "$REPO_DIR/bench/side/side_${sf}.txt" "$((700000 + i))" "$LINES_PER_FILE"
  run_ai_checkpoint
  g add -A bench/side
  g commit -m "bench(ai): side commit $i" >/dev/null

  if (( i % 20 == 0 || i == SIDE_COMMITS )); then
    echo "  side commits: $i/$SIDE_COMMITS"
  fi
done
SIDE_TIP="$(g rev-parse HEAD)"

g checkout -B bench-feature-merge "$FEATURE_TIP" >/dev/null
g merge --no-ff --no-edit bench-side >/dev/null
MERGE_FEATURE_TIP="$(g rev-parse HEAD)"

echo
echo "Running rebase scenarios..."
echo -e "scenario\tstatus\tduration_s\tsaved_logs\thead_note" >"$RESULTS_TSV"
{
  echo "git-ai nasty rebase benchmark summary"
  echo "repo_url: $REPO_URL"
  echo "repo_dir: $REPO_DIR"
  echo "default_branch: $DEFAULT_BRANCH"
  echo "base_sha: $BASE_SHA"
  echo "feature_tip: $FEATURE_TIP"
  echo "main_tip: $MAIN_TIP"
  echo "side_tip: $SIDE_TIP"
  echo "merge_feature_tip: $MERGE_FEATURE_TIP"
  echo "feature_commits: $FEATURE_COMMITS"
  echo "main_commits: $MAIN_COMMITS"
  echo "side_commits: $SIDE_COMMITS"
  echo "files: $FILES"
  echo "lines_per_file: $LINES_PER_FILE"
  echo "burst_every: $BURST_EVERY"
  echo
} >"$SUMMARY_FILE"

# Scenario 1: linear heavy rebase
g branch -f bench-feature-linear "$FEATURE_TIP" >/dev/null
run_rebase_scenario "linear" "bench-feature-linear" \
  rebase bench-main bench-feature-linear

# Scenario 2: rebase --onto on a large subset of the stack
g branch -f bench-feature-onto "$FEATURE_TIP" >/dev/null
run_rebase_scenario "onto" "bench-feature-onto" \
  rebase --onto bench-main "$BASE_SHA" bench-feature-onto

# Scenario 3: merge-preserving rebase of a branch with many side commits
g branch -f bench-feature-rm "$MERGE_FEATURE_TIP" >/dev/null
run_rebase_scenario "rebase_merges" "bench-feature-rm" \
  rebase --rebase-merges bench-main bench-feature-rm

echo
echo "=== Benchmark complete ==="
echo "Summary: $SUMMARY_FILE"
echo "Results TSV: $RESULTS_TSV"
echo "Logs dir: $LOG_DIR"

column -t -s $'\t' "$RESULTS_TSV" || cat "$RESULTS_TSV"
