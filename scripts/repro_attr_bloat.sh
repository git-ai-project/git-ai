#!/usr/bin/env bash
set -euo pipefail

# Repro script: create a large file, apply many small edits, checkpoint with mock_ai,
# then commit via git-ai to stress attribution range growth and memory usage.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Locate git-ai binary (override with GIT_AI_BIN=...).
GIT_AI_BIN="${GIT_AI_BIN:-}"
if [[ -z "${GIT_AI_BIN}" ]]; then
  if [[ -x "${REPO_ROOT}/target/debug/git-ai" ]]; then
    GIT_AI_BIN="${REPO_ROOT}/target/debug/git-ai"
  elif [[ -x "${REPO_ROOT}/target/release/git-ai" ]]; then
    GIT_AI_BIN="${REPO_ROOT}/target/release/git-ai"
  fi
fi

if [[ -z "${GIT_AI_BIN}" ]]; then
  echo "ERROR: git-ai binary not found. Build it first (e.g., cargo build) and/or set GIT_AI_BIN."
  exit 1
fi

# Tuning knobs (override via env)
LINES="${LINES:-200000}"           # number of lines in the large file
LINE_LEN="${LINE_LEN:-120}"        # approx chars per line
ITERATIONS="${ITERATIONS:-50}"     # number of edit+checkpoint cycles
EDIT_STRIDE="${EDIT_STRIDE:-10}"   # edit every Nth line each iteration
REPORT_EVERY="${REPORT_EVERY:-10}" # print attribution stats every N iterations
KEEP_DIR="${KEEP_DIR:-0}"          # set to 1 to keep the temp repo

# time(1) command for peak RSS (macOS: -l, Linux: -v)
TIME_CMD=""
if command -v /usr/bin/time >/dev/null 2>&1; then
  if [[ "$(uname -s)" == "Darwin" ]]; then
    TIME_CMD="/usr/bin/time -l"
  else
    TIME_CMD="/usr/bin/time -v"
  fi
fi

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/git-ai-attr-bloat-XXXXXX")"
cleanup() {
  if [[ "${KEEP_DIR}" == "1" ]]; then
    echo "Repo kept at: ${WORKDIR}"
  else
    rm -rf "${WORKDIR}"
  fi
}
trap cleanup EXIT

cd "${WORKDIR}"
git init -q
git config user.email "repro@example.com"
git config user.name "Repro"

echo "Generating ${LINES} lines..."
python3 - <<PY
import random, string
lines = int("${LINES}")
line_len = int("${LINE_LEN}")
def make_line(i):
    prefix = f"{i:07d} "
    payload_len = max(1, line_len - len(prefix))
    payload = ("A" * payload_len)
    return prefix + payload
with open("big.txt", "w", encoding="utf-8") as f:
    for i in range(lines):
        f.write(make_line(i) + "\n")
PY

git add big.txt
git commit -q -m "init"

echo "Starting ${ITERATIONS} edit+checkpoint cycles..."
for i in $(seq 1 "${ITERATIONS}"); do
  python3 - <<PY
import sys
stride = int("${EDIT_STRIDE}")
iter_no = int("${i}")
out = []
with open("big.txt", "r", encoding="utf-8") as f:
    for idx, line in enumerate(f):
        if idx % stride == 0:
            # Replace a small token near the end to create many small diffs.
            line = line.rstrip("\n")
            marker = f" E{iter_no:04d}"
            if len(line) >= len(marker) + 1:
                line = line[:-(len(marker)+1)] + " " + marker
            else:
                line = line + " " + marker
            line = line + "\n"
        out.append(line)
with open("big.txt", "w", encoding="utf-8") as f:
    f.writelines(out)
PY

  "${GIT_AI_BIN}" checkpoint mock_ai big.txt >/dev/null

  if (( i % REPORT_EVERY == 0 )); then
    base_sha="$(git rev-parse HEAD)"
    checkpoints="${WORKDIR}/.git/ai/working_logs/${base_sha}/checkpoints.jsonl"
    size_bytes=0
    if [[ -f "${checkpoints}" ]]; then
      size_bytes=$(wc -c < "${checkpoints}")
    fi
    python3 - <<PY
import json, os
path = "${checkpoints}"
total_char = 0
total_line = 0
if os.path.exists(path):
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            obj = json.loads(line)
            for entry in obj.get("entries", []):
                total_char += len(entry.get("attributions", []))
                total_line += len(entry.get("line_attributions", []))
print(f"[iter ${i}] checkpoints.jsonl size={int(${size_bytes})/1024/1024:.2f}MB "
      f"char_ranges={total_char} line_ranges={total_line}")
PY
  fi
done

git add big.txt
echo "Committing with git-ai (this runs post-commit processing)..."
if [[ -n "${TIME_CMD}" ]]; then
  env GIT_AI=git ${TIME_CMD} "${GIT_AI_BIN}" commit -m "stress commit"
else
  GIT_AI=git "${GIT_AI_BIN}" commit -m "stress commit"
fi

echo "Done."
