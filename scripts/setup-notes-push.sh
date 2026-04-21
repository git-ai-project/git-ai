#!/usr/bin/env bash
# setup-notes-push.sh
# -------------------------------------------------------------------
# Run this once per dev machine to configure git so that
#   git push
# automatically also pushes refs/notes/ai to the remote.
#
# Usage:
#   ./scripts/setup-notes-push.sh              # configures 'origin'
#   ./scripts/setup-notes-push.sh upstream     # configures a different remote
# -------------------------------------------------------------------
set -euo pipefail

REMOTE="${1:-origin}"

# Verify we're inside a git repo
git rev-parse --git-dir > /dev/null

echo "Configuring git to push refs/notes/ai to remote '$REMOTE' ..."

# Add a push refspec so that 'git push $REMOTE' also sends notes
git config --add "remote.$REMOTE.push" '+refs/heads/*:refs/heads/*'
git config --add "remote.$REMOTE.push" '+refs/notes/*:refs/notes/*'

# Also configure fetch so 'git fetch' / 'git pull' retrieves other
# developers' notes (merging them locally)
git config --add "remote.$REMOTE.fetch" '+refs/notes/*:refs/notes/*'

# Ensure git knows how to merge notes refs automatically
git config notes.mergeStrategy "cat_sort_uniq"
git config notes.rewriteRef "refs/notes/ai"
git config notes.rewriteMode "concatenate"

echo ""
echo "Done. Your git push/fetch for remote '$REMOTE' will now"
echo "include refs/notes/ai automatically."
echo ""
echo "To push existing notes right now, run:"
echo "  git push $REMOTE refs/notes/ai"
echo ""
echo "Share this script with your teammates:"
echo "  ./scripts/setup-notes-push.sh"
