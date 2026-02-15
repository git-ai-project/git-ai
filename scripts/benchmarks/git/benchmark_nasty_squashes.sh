#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Benchmark git-ai squash performance against plain git on heavy synthetic commit stacks.

Usage:
  benchmark_nasty_squashes.sh [options]

Options:
  --repo-url <url>             OSS repo to clone (default: https://github.com/python/cpython.git)
  --work-root <path>           Working directory (default: /tmp/git-ai-nasty-squash-<timestamp>)
  --feature-commits <n>        Number of AI feature commits (default: 180)
  --main-commits <n>           Number of upstream target-branch commits (default: 60)
  --files <n>                  Number of generated feature files (default: 30)
  --lines-per-file <n>         Lines per generated file (default: 1500)
  --burst-every <n>            Every Nth feature commit rewrites all generated files (default: 20)
  --git-bin <path>             Wrapped git binary (default: wrapper next to git-ai, else PATH git)
  --plain-git-bin <path>       Plain git binary (default: /usr/bin/git, else first non-wrapper git in PATH)
  --git-ai-bin <path>          git-ai binary (default: PATH git-ai)
  --skip-clone                 Reuse existing clone in <work-root>/repo
  -h, --help                   Show help

Outputs:
  - Logs: <work-root>/logs/*.log
  - Summary: <work-root>/summary.txt
  - Results TSV: <work-root>/results.tsv
EOF
}

REPO_URL="https://github.com/python/cpython.git"
WORK_ROOT=""
FEATURE_COMMITS=180
MAIN_COMMITS=60
FILES=30
LINES_PER_FILE=1500
BURST_EVERY=20
SKIP_CLONE=0
GIT_BIN=""
PLAIN_GIT_BIN=""
GIT_AI_BIN=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-url) REPO_URL="$2"; shift 2 ;;
    --work-root) WORK_ROOT="$2"; shift 2 ;;
    --feature-commits) FEATURE_COMMITS="$2"; shift 2 ;;
    --main-commits) MAIN_COMMITS="$2"; shift 2 ;;
    --files) FILES="$2"; shift 2 ;;
    --lines-per-file) LINES_PER_FILE="$2"; shift 2 ;;
    --burst-every) BURST_EVERY="$2"; shift 2 ;;
    --git-bin) GIT_BIN="$2"; shift 2 ;;
    --plain-git-bin) PLAIN_GIT_BIN="$2"; shift 2 ;;
    --git-ai-bin) GIT_AI_BIN="$2"; shift 2 ;;
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

detect_plain_git_bin() {
  local wrapped="$1"

  if [[ -x /usr/bin/git && "/usr/bin/git" != "$wrapped" ]]; then
    echo "/usr/bin/git"
    return
  fi

  local candidate
  while IFS= read -r candidate; do
    if [[ -n "$candidate" && "$candidate" != "$wrapped" ]]; then
      echo "$candidate"
      return
    fi
  done < <(which -a git 2>/dev/null | awk '!seen[$0]++')

  echo "$wrapped"
}

if [[ -z "$PLAIN_GIT_BIN" ]]; then
  PLAIN_GIT_BIN="$(detect_plain_git_bin "$GIT_BIN")"
fi

if [[ -z "$WORK_ROOT" ]]; then
  WORK_ROOT="${TMPDIR:-/tmp}/git-ai-nasty-squash-$(date +%Y%m%d-%H%M%S)"
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

extract_perf_field() {
  local log_file="$1"
  local command="$2"
  local field="$3"
  python3 - "$log_file" "$command" "$field" <<'PY'
import json
import sys

path, cmd, field = sys.argv[1], sys.argv[2], sys.argv[3]
last = None
try:
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            if "[git-ai (perf-json)]" not in line:
                continue
            start = line.find("{")
            if start < 0:
                continue
            payload = line[start:].strip()
            try:
                obj = json.loads(payload)
            except Exception:
                continue
            if obj.get("command") == cmd:
                last = obj
except FileNotFoundError:
    pass

if last is not None and field in last:
    print(last[field])
PY
}

g_plain() {
  "$PLAIN_GIT_BIN" -C "$REPO_DIR" "$@"
}

g_ai() {
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

ensure_clean_state() {
  g_ai rebase --abort >/dev/null 2>&1 || true
  g_ai merge --abort >/dev/null 2>&1 || true
  g_ai am --abort >/dev/null 2>&1 || true
  g_ai cherry-pick --abort >/dev/null 2>&1 || true
}

run_mode_squash() {
  local scenario="$1"
  local mode="$2"
  local source_branch="$3"
  local target_branch="$4"

  local run_branch="bench-${scenario}-${mode}"
  local merge_log="$LOG_DIR/${scenario}-${mode}-merge.log"
  local merge_clean="$LOG_DIR/${scenario}-${mode}-merge.clean.log"
  local commit_log="$LOG_DIR/${scenario}-${mode}-commit.log"
  local commit_clean="$LOG_DIR/${scenario}-${mode}-commit.clean.log"

  local git_cmd
  if [[ "$mode" == "plain" ]]; then
    git_cmd="$PLAIN_GIT_BIN"
  else
    git_cmd="$GIT_BIN"
  fi

  ensure_clean_state
  "$git_cmd" -C "$REPO_DIR" checkout -B "$run_branch" "$target_branch" >/dev/null

  local merge_status="ok"
  local commit_status="ok"

  local merge_start_ns merge_end_ns merge_s
  merge_start_ns="$(now_ns)"
  if [[ "$mode" == "plain" ]]; then
    if "$git_cmd" -C "$REPO_DIR" merge --squash "$source_branch" >"$merge_log" 2>&1; then
      merge_status="ok"
    else
      merge_status="fail"
    fi
  else
    if GIT_AI_DEBUG=1 GIT_AI_DEBUG_PERFORMANCE=2 "$git_cmd" -C "$REPO_DIR" merge --squash "$source_branch" >"$merge_log" 2>&1; then
      merge_status="ok"
    else
      merge_status="fail"
    fi
  fi
  merge_end_ns="$(now_ns)"
  merge_s="$(seconds_from_ns_delta "$merge_start_ns" "$merge_end_ns")"

  local commit_start_ns commit_end_ns commit_s
  commit_start_ns="$(now_ns)"
  if [[ "$merge_status" == "ok" ]]; then
    if [[ "$mode" == "plain" ]]; then
      if "$git_cmd" -C "$REPO_DIR" commit -m "bench(squash): $scenario $mode" >"$commit_log" 2>&1; then
        commit_status="ok"
      else
        commit_status="fail"
      fi
    else
      if GIT_AI_DEBUG=1 GIT_AI_DEBUG_PERFORMANCE=2 "$git_cmd" -C "$REPO_DIR" commit -m "bench(squash): $scenario $mode" >"$commit_log" 2>&1; then
        commit_status="ok"
      else
        commit_status="fail"
      fi
    fi
  else
    commit_status="skip"
    : >"$commit_log"
  fi
  commit_end_ns="$(now_ns)"
  commit_s="$(seconds_from_ns_delta "$commit_start_ns" "$commit_end_ns")"

  strip_ansi_file "$merge_log" "$merge_clean"
  strip_ansi_file "$commit_log" "$commit_clean"

  local overall_status="ok"
  if [[ "$merge_status" != "ok" || "$commit_status" != "ok" ]]; then
    overall_status="fail"
    ensure_clean_state
  fi

  local head_sha note_state
  head_sha="$("$git_cmd" -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || true)"
  if [[ "$mode" == "ai" && -n "$head_sha" ]] && g_ai notes --ref=ai show "$head_sha" >/dev/null 2>&1; then
    note_state="yes"
  else
    note_state="no"
  fi

  local merge_git_ms="-" merge_pre_ms="-" merge_post_ms="-"
  local commit_git_ms="-" commit_pre_ms="-" commit_post_ms="-"
  if [[ "$mode" == "ai" ]]; then
    merge_git_ms="$(extract_perf_field "$merge_clean" "merge" "git_duration_ms")"
    merge_pre_ms="$(extract_perf_field "$merge_clean" "merge" "pre_command_duration_ms")"
    merge_post_ms="$(extract_perf_field "$merge_clean" "merge" "post_command_duration_ms")"
    commit_git_ms="$(extract_perf_field "$commit_clean" "commit" "git_duration_ms")"
    commit_pre_ms="$(extract_perf_field "$commit_clean" "commit" "pre_command_duration_ms")"
    commit_post_ms="$(extract_perf_field "$commit_clean" "commit" "post_command_duration_ms")"
  fi

  local total_s
  total_s="$(python3 - "$merge_s" "$commit_s" <<'PY'
import sys
print(f"{float(sys.argv[1]) + float(sys.argv[2]):.3f}")
PY
)"

  printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
    "$scenario" "$mode" "$overall_status" "$merge_s" "$commit_s" "$total_s" \
    "$merge_git_ms" "$merge_pre_ms" "$merge_post_ms" \
    "$commit_git_ms" "$commit_pre_ms" "$commit_post_ms" \
    "$note_state" "$source_branch" "$target_branch" >>"$RESULTS_TSV"

  {
    echo "scenario: $scenario"
    echo "mode: $mode"
    echo "status: $overall_status"
    echo "source_branch: $source_branch"
    echo "target_branch: $target_branch"
    echo "result_branch: $run_branch"
    echo "head_sha: ${head_sha:-<none>}"
    echo "merge_seconds: $merge_s"
    echo "commit_seconds: $commit_s"
    echo "total_seconds: $total_s"
    if [[ "$mode" == "ai" ]]; then
      echo "merge_git_duration_ms: ${merge_git_ms:-<none>}"
      echo "merge_pre_duration_ms: ${merge_pre_ms:-<none>}"
      echo "merge_post_duration_ms: ${merge_post_ms:-<none>}"
      echo "commit_git_duration_ms: ${commit_git_ms:-<none>}"
      echo "commit_pre_duration_ms: ${commit_pre_ms:-<none>}"
      echo "commit_post_duration_ms: ${commit_post_ms:-<none>}"
    fi
    echo "head_has_ai_note: $note_state"
    echo "merge_log: $merge_log"
    echo "commit_log: $commit_log"
    echo
  } >>"$SUMMARY_FILE"

  echo "scenario=$scenario mode=$mode status=$overall_status merge=${merge_s}s commit=${commit_s}s total=${total_s}s note=$note_state"
  if [[ "$mode" == "ai" ]]; then
    echo "  merge breakdown ms: pre=${merge_pre_ms:-?} git=${merge_git_ms:-?} post=${merge_post_ms:-?}"
    echo "  commit breakdown ms: pre=${commit_pre_ms:-?} git=${commit_git_ms:-?} post=${commit_post_ms:-?}"
  fi
}

echo "=== git-ai nasty squash benchmark ==="
echo "repo_url=$REPO_URL"
echo "work_root=$WORK_ROOT"
echo "repo_dir=$REPO_DIR"
echo "plain_git_bin=$PLAIN_GIT_BIN"
echo "git_bin=$GIT_BIN"
echo "git_ai_bin=$GIT_AI_BIN"
echo "feature_commits=$FEATURE_COMMITS main_commits=$MAIN_COMMITS files=$FILES lines_per_file=$LINES_PER_FILE burst_every=$BURST_EVERY"

if [[ "$SKIP_CLONE" -eq 0 ]]; then
  rm -rf "$REPO_DIR"
  echo "Cloning repo..."
  "$PLAIN_GIT_BIN" clone --depth 1 "$REPO_URL" "$REPO_DIR" >/dev/null
fi

if [[ ! -d "$REPO_DIR/.git" ]]; then
  echo "error: repo missing at $REPO_DIR" >&2
  exit 1
fi

DEFAULT_BRANCH="$("$PLAIN_GIT_BIN" -C "$REPO_DIR" rev-parse --abbrev-ref origin/HEAD 2>/dev/null | sed 's|^origin/||')"
if [[ -z "$DEFAULT_BRANCH" || "$DEFAULT_BRANCH" == "HEAD" ]]; then
  if "$PLAIN_GIT_BIN" -C "$REPO_DIR" rev-parse --verify origin/main >/dev/null 2>&1; then
    DEFAULT_BRANCH="main"
  elif "$PLAIN_GIT_BIN" -C "$REPO_DIR" rev-parse --verify origin/master >/dev/null 2>&1; then
    DEFAULT_BRANCH="master"
  else
    DEFAULT_BRANCH="$("$PLAIN_GIT_BIN" -C "$REPO_DIR" rev-parse --abbrev-ref HEAD)"
  fi
fi
echo "default_branch=$DEFAULT_BRANCH"

g_plain config user.name "git-ai bench"
g_plain config user.email "bench@git-ai.local"
g_plain config commit.gpgsign false
g_plain config gc.auto 0

g_plain checkout -B bench-main-base "origin/$DEFAULT_BRANCH" >/dev/null

echo "Seeding generated files..."
for f in $(seq 1 "$FILES"); do
  generate_file "$REPO_DIR/bench/generated/file_${f}.txt" "$((1000 + f))" "$LINES_PER_FILE"
done
g_plain add -A bench/generated
g_plain commit -m "bench: seed generated files" >/dev/null
BASE_SHA="$(g_plain rev-parse HEAD)"

echo "Creating feature branch with heavy AI history..."
g_plain checkout -B bench-feature "$BASE_SHA" >/dev/null
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
  g_ai add -A bench/generated
  g_ai commit -m "bench(ai): feature commit $i" >/dev/null

  if (( i % 25 == 0 || i == FEATURE_COMMITS )); then
    echo "  feature commits: $i/$FEATURE_COMMITS"
  fi
done
FEATURE_TIP="$(g_plain rev-parse HEAD)"

echo "Creating diverged target branch with upstream churn..."
g_plain checkout -B bench-main-diverged "$BASE_SHA" >/dev/null
for i in $(seq 1 "$MAIN_COMMITS"); do
  uf=$(( (i - 1) % 3 + 1 ))
  generate_file "$REPO_DIR/bench/upstream/upstream_${uf}.txt" "$((900000 + i))" "$((LINES_PER_FILE / 2))"
  g_plain add -A bench/upstream
  g_plain commit -m "bench(main): upstream commit $i" >/dev/null
  if (( i % 20 == 0 || i == MAIN_COMMITS )); then
    echo "  main commits: $i/$MAIN_COMMITS"
  fi
done
MAIN_DIVERGED_TIP="$(g_plain rev-parse HEAD)"

echo -e "scenario\tmode\tstatus\tmerge_s\tcommit_s\ttotal_s\tmerge_git_ms\tmerge_pre_ms\tmerge_post_ms\tcommit_git_ms\tcommit_pre_ms\tcommit_post_ms\thead_note\tsource\ttarget" >"$RESULTS_TSV"
{
  echo "git-ai nasty squash benchmark summary"
  echo "repo_url: $REPO_URL"
  echo "repo_dir: $REPO_DIR"
  echo "default_branch: $DEFAULT_BRANCH"
  echo "base_sha: $BASE_SHA"
  echo "feature_tip: $FEATURE_TIP"
  echo "main_diverged_tip: $MAIN_DIVERGED_TIP"
  echo "feature_commits: $FEATURE_COMMITS"
  echo "main_commits: $MAIN_COMMITS"
  echo "files: $FILES"
  echo "lines_per_file: $LINES_PER_FILE"
  echo "burst_every: $BURST_EVERY"
  echo "plain_git_bin: $PLAIN_GIT_BIN"
  echo "git_bin: $GIT_BIN"
  echo
} >"$SUMMARY_FILE"

echo
echo "Running squash scenarios..."
run_mode_squash "linear" "plain" "bench-feature" "$BASE_SHA"
run_mode_squash "linear" "ai" "bench-feature" "$BASE_SHA"

run_mode_squash "diverged" "plain" "bench-feature" "$MAIN_DIVERGED_TIP"
run_mode_squash "diverged" "ai" "bench-feature" "$MAIN_DIVERGED_TIP"

echo
echo "=== Benchmark complete ==="
echo "Summary: $SUMMARY_FILE"
echo "Results TSV: $RESULTS_TSV"
echo "Logs dir: $LOG_DIR"
column -t -s $'\t' "$RESULTS_TSV" || cat "$RESULTS_TSV"

echo
echo "== Slowdown summary (ai vs plain) =="
python3 - "$RESULTS_TSV" <<'PY'
import csv
import sys

path = sys.argv[1]
rows = list(csv.DictReader(open(path, "r", encoding="utf-8"), delimiter="\t"))
by = {}
for r in rows:
    key = r["scenario"]
    by.setdefault(key, {})[r["mode"]] = r

for scenario, modes in by.items():
    if "plain" not in modes or "ai" not in modes:
        continue
    p = modes["plain"]
    a = modes["ai"]
    try:
        plain_total = float(p["total_s"])
        ai_total = float(a["total_s"])
        plain_merge = float(p["merge_s"])
        ai_merge = float(a["merge_s"])
        plain_commit = float(p["commit_s"])
        ai_commit = float(a["commit_s"])
    except Exception:
        continue

    ratio_total = ai_total / plain_total if plain_total > 0 else float("inf")
    ratio_merge = ai_merge / plain_merge if plain_merge > 0 else float("inf")
    ratio_commit = ai_commit / plain_commit if plain_commit > 0 else float("inf")

    merge_post = a.get("merge_post_ms", "-")
    commit_post = a.get("commit_post_ms", "-")
    print(
        f"{scenario}: total={ratio_total:.1f}x "
        f"(merge={ratio_merge:.1f}x, commit={ratio_commit:.1f}x), "
        f"ai merge_post_ms={merge_post}, ai commit_post_ms={commit_post}"
    )
PY
