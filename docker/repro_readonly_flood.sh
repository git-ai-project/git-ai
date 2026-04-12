#!/usr/bin/env bash
# repro_readonly_flood.sh
#
# Reproduces and validates the Zed IDE readonly-flood daemon performance fix.
#
# Context:
#   Zed IDE observed sending ~40 git readonly invocations/sec to the daemon
#   (status, diff, stash list, worktree list, cat-file, for-each-ref, show).
#   Before the fix, all events entered the serial ingest queue, creating >1 min
#   of backlog before any mutating command's checkpoint could complete.
#
# What this script does:
#   1. Sets up a git repo and starts the git-ai daemon
#   2. Floods the daemon with 400 readonly commands in rapid succession
#      (simulating ~40/sec × 10s of Zed activity)
#   3. Immediately runs a git commit and times the checkpoint
#   4. Reports pass/fail based on checkpoint latency:
#      - < 5 s  → FIX WORKING   (readonly events discarded before queue)
#      - > 30 s → FIX MISSING   (queue backlogged with readonly events)
#
# Expected output with the fix:
#   [PASS] Checkpoint after 400-event readonly flood completed in Xs (< 5s)
#
set -euo pipefail

DAEMON_HOME="${HOME}/.git-ai"
TRACE_SOCK="${DAEMON_HOME}/internal/daemon/trace2.sock"
CHECKPOINT_TIMEOUT=60  # seconds before we give up waiting for checkpoint
FLOOD_COUNT=400        # readonly commands to fire before the mutating commit
PARALLEL_JOBS=10       # concurrency of the readonly flood (like Zed)

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log()  { echo -e "[$(date +%H:%M:%S)] $*"; }
pass() { echo -e "${GREEN}[PASS]${NC} $*"; }
fail() { echo -e "${RED}[FAIL]${NC} $*"; exit 1; }
info() { echo -e "${YELLOW}[INFO]${NC} $*"; }

# ---------------------------------------------------------------------------
# 1. Create a throwaway git repository
# ---------------------------------------------------------------------------
REPO=$(mktemp -d /tmp/readonly-flood-repo.XXXXXX)
trap 'rm -rf "$REPO"; git-ai daemon shutdown 2>/dev/null || true' EXIT

log "Setting up git repo at $REPO"
git -C "$REPO" init -q
git -C "$REPO" commit --allow-empty -q -m "init"

# ---------------------------------------------------------------------------
# 2. Start the daemon and wait for the socket to appear
# ---------------------------------------------------------------------------
log "Starting git-ai daemon..."
git-ai daemon start &>/dev/null || true

# Wait up to 10s for the socket
for i in $(seq 1 20); do
    if [ -S "$TRACE_SOCK" ]; then break; fi
    sleep 0.5
done
if [ ! -S "$TRACE_SOCK" ]; then
    fail "Daemon socket not found at $TRACE_SOCK after 10s"
fi
log "Daemon ready (socket: $TRACE_SOCK)"

# Build the trace2 event target string
TRACE2_TARGET="af_unix:stream:${TRACE_SOCK}"

# ---------------------------------------------------------------------------
# 3. Verify the socket accepts connections
# ---------------------------------------------------------------------------
log "Verifying daemon accepts connections..."
if ! echo "" | socat - UNIX-CONNECT:"$TRACE_SOCK" 2>/dev/null; then
    log "Note: socat connect returned non-zero but socket exists — proceeding"
fi

# ---------------------------------------------------------------------------
# 4. Flood with readonly commands (Zed-style)
# ---------------------------------------------------------------------------
info "Flooding daemon with ${FLOOD_COUNT} readonly commands (parallelism=${PARALLEL_JOBS})"
info "Simulates Zed IDE git panel at ~40 commands/sec"

FLOOD_COMMANDS=(
    "status --porcelain=v2"
    "diff --stat"
    "for-each-ref --format=%(refname:short)"
    "stash list"
    "worktree list --porcelain"
    "show --stat HEAD"
    "log --oneline -5"
)

flood_start_ns=$(date +%s%N)

flood_worker() {
    local start_i=$1
    local count=$2
    local cmd_count=${#FLOOD_COMMANDS[@]}
    for ((i=start_i; i<start_i+count; i++)); do
        local cmd="${FLOOD_COMMANDS[$((i % cmd_count))]}"
        # Use GIT_TRACE2_EVENT to route events to daemon; suppress actual git output
        GIT_TRACE2_EVENT="$TRACE2_TARGET" \
            git -C "$REPO" $cmd &>/dev/null || true
    done
}

per_worker=$(( FLOOD_COUNT / PARALLEL_JOBS ))
pids=()
for ((w=0; w<PARALLEL_JOBS; w++)); do
    flood_worker $((w * per_worker)) $per_worker &
    pids+=($!)
done
for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done

flood_end_ns=$(date +%s%N)
flood_ms=$(( (flood_end_ns - flood_start_ns) / 1000000 ))
log "Flood complete: ${FLOOD_COUNT} commands in ${flood_ms}ms"

# ---------------------------------------------------------------------------
# 5. Time a checkpoint immediately after the flood
# ---------------------------------------------------------------------------
log "Running git commit + checkpoint (measuring latency)..."

git -C "$REPO" commit --allow-empty -q -m "post-flood commit"

checkpoint_start=$SECONDS
timeout "${CHECKPOINT_TIMEOUT}" git-ai checkpoint 2>/dev/null || {
    elapsed=$((SECONDS - checkpoint_start))
    fail "Checkpoint timed out after ${elapsed}s — queue likely backlogged with readonly events"
}
checkpoint_elapsed=$((SECONDS - checkpoint_start))

# ---------------------------------------------------------------------------
# 6. Report results
# ---------------------------------------------------------------------------
echo ""
echo "======================================================="
echo "  Readonly Flood Repro Results"
echo "======================================================="
echo "  Flood:       ${FLOOD_COUNT} readonly commands in ${flood_ms}ms"
echo "  Checkpoint:  ${checkpoint_elapsed}s"
echo ""

if [ "$checkpoint_elapsed" -lt 5 ]; then
    pass "Checkpoint after ${FLOOD_COUNT}-event readonly flood completed in ${checkpoint_elapsed}s (< 5s)"
    echo ""
    echo "  The fix is working: readonly events are discarded before reaching"
    echo "  the serial ingest queue, so the checkpoint sees no backlog."
elif [ "$checkpoint_elapsed" -lt 30 ]; then
    echo -e "${YELLOW}[WARN]${NC} Checkpoint took ${checkpoint_elapsed}s — borderline (expected < 5s with fix)"
else
    fail "Checkpoint took ${checkpoint_elapsed}s — the serial ingest queue is likely backlogged"
fi
