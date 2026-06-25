#!/usr/bin/env bash
#
# Reproduces the issue #1657 / #1287 daemon-lock failure mode in a controlled
# way: the daemon lock file is owned by a *different* (here: root) user, so the
# normal user's git-ai cannot open it. The OS returns a permission error -- NOT
# lock contention -- yet pre-fix git-ai reports "daemon startup blocked: lock
# held", sending users chasing a process that does not exist.
#
# This is the Linux analogue of the Windows scenario in the bug report, where an
# Administrator install leaves C:\Users\<user>\.git-ai\internal owned by an
# elevated account and the normal-user agent hook can no longer start the daemon.
#
# Run via the Dockerfile in this directory. Exit code 0 = scenario reproduced.

set -uo pipefail

GIT_AI_BIN="${GIT_AI_BIN:-/src/target/debug/git-ai}"
DEV_HOME="/home/dev"
INTERNAL_DIR="${DEV_HOME}/.git-ai/internal/daemon"
LOCK_FILE="${INTERNAL_DIR}/daemon.lock"

hr() { printf '%s\n' "------------------------------------------------------------"; }

echo "git-ai binary: ${GIT_AI_BIN}"
"${GIT_AI_BIN}" version 2>/dev/null | head -1 || true
hr

# 1) Simulate the elevated install: create the lock file tree owned by root,
#    inside the normal user's home, with the lock file readable but NOT writable
#    by the normal user. No daemon is running -- so any "lock held" report is
#    definitionally wrong; the only obstacle is file ownership/permissions.
echo "[setup] creating root-owned daemon lock inside ${DEV_HOME} (simulating an"
echo "        Administrator/sudo install); NO daemon process is running."
mkdir -p "${INTERNAL_DIR}"
: > "${LOCK_FILE}"
chown -R root:root "${DEV_HOME}/.git-ai"
chmod 0755 "${DEV_HOME}/.git-ai" "${DEV_HOME}/.git-ai/internal" "${INTERNAL_DIR}"
chmod 0644 "${LOCK_FILE}"        # readable, but unwritable by 'dev' -> open(write) = EACCES
# The home dir itself stays owned by dev so HOME resolves normally.
chown dev:dev "${DEV_HOME}"

echo "[setup] lock file state:"
ls -ld "${INTERNAL_DIR}" "${LOCK_FILE}" | sed 's/^/        /'
echo "[setup] processes named git-ai (expect none):"
( pgrep -a git-ai || echo "        <none>" ) | sed 's/^/        /'
hr

# 2) As the normal user 'dev', ask git-ai to start the daemon. This drives the
#    same code path the agent hooks hit (ensure the daemon is up).
echo "[run] starting daemon as unprivileged user 'dev'..."
OUT="$(sudo -u dev env HOME="${DEV_HOME}" GIT_AI_DEBUG=0 "${GIT_AI_BIN}" bg start 2>&1)"
RC=$?
echo "${OUT}" | sed 's/^/      /'
echo "[run] exit code: ${RC}"
hr

# 3) Classify what git-ai reported.
echo "[verdict]"
if echo "${OUT}" | grep -qiE "inaccessible|permission|created by a different or elevated user"; then
  echo "  ✅ FIXED BEHAVIOR: git-ai correctly reports a permission/ownership"
  echo "     problem and tells the user how to recover. It does not pretend a"
  echo "     phantom process holds the lock."
  exit 0
elif echo "${OUT}" | grep -qiE "lock held"; then
  echo "  ❌ BUGGY BEHAVIOR (pre-fix): git-ai reports 'lock held' even though no"
  echo "     process holds the lock -- the real cause is that the lock file is"
  echo "     owned by another user. This is the #1657 / #1287 masking bug:"
  echo "     EACCES from open() is collapsed into 'contended'."
  exit 0
else
  echo "  ⚠️  Unexpected output -- scenario did not reproduce cleanly."
  echo "     (Did the build succeed? Is GIT_AI_BIN correct?)"
  exit 1
fi
