#!/usr/bin/env bash
set -uo pipefail

# Retry logic for flaky tests in daemon and wrapper-daemon modes.
# Only re-runs failed tests (not the full suite) for speed.
# Exits 0 with a warning if flaky tests pass on retry.

TEST_THREADS="${1:-4}"
TEST_MODE="${GIT_AI_TEST_GIT_MODE:-}"

run_cargo_test() {
  local filter="${1:-}"
  local extra_args=""
  if [ -n "$filter" ]; then
    extra_args="--exact"
  fi
  cargo test $filter -- --test-threads="$TEST_THREADS" $extra_args
}

# Run the full test suite, capturing output
OUTPUT_FILE=$(mktemp)
cargo test -- --test-threads="$TEST_THREADS" 2>&1 | tee "$OUTPUT_FILE"
FIRST_EXIT=${PIPESTATUS[0]}

if [ "$FIRST_EXIT" -eq 0 ]; then
  rm -f "$OUTPUT_FILE"
  exit 0
fi

# Only retry for daemon and wrapper-daemon modes
if [ "$TEST_MODE" != "daemon" ] && [ "$TEST_MODE" != "wrapper-daemon" ]; then
  echo "::error::Tests failed in '$TEST_MODE' mode (retry not enabled for this mode)"
  rm -f "$OUTPUT_FILE"
  exit 1
fi

# Parse failed test names from the output.
# cargo test prints a failures section like:
#   failures:
#       test_name_1
#       test_name_2
# We extract those names.
FAILED_TESTS=$(awk '
  /^failures:$/ { in_failures=1; next }
  in_failures && /^$/ { in_failures=0; next }
  in_failures && /^test result:/ { in_failures=0; next }
  in_failures && /^[[:space:]]+[a-zA-Z_]/ { gsub(/^[[:space:]]+/, ""); print }
' "$OUTPUT_FILE")

rm -f "$OUTPUT_FILE"

if [ -z "$FAILED_TESTS" ]; then
  echo "::error::Tests failed but could not parse failed test names for retry"
  exit 1
fi

FAILED_COUNT=$(echo "$FAILED_TESTS" | wc -l | tr -d ' ')
echo ""
echo "::warning::$FAILED_COUNT test(s) failed on first run in '$TEST_MODE' mode. Retrying individually..."
echo ""

# Retry each failed test individually
STILL_FAILING=""
PASSED_ON_RETRY=""

while IFS= read -r test_name; do
  [ -z "$test_name" ] && continue
  echo "--- Retrying: $test_name ---"
  if cargo test "$test_name" -- --test-threads=1 --exact; then
    PASSED_ON_RETRY="${PASSED_ON_RETRY}${test_name}\n"
  else
    STILL_FAILING="${STILL_FAILING}${test_name}\n"
  fi
done <<< "$FAILED_TESTS"

echo ""

if [ -n "$STILL_FAILING" ]; then
  echo "::error::The following tests failed even on retry:"
  echo -e "$STILL_FAILING" | while IFS= read -r t; do
    [ -n "$t" ] && echo "  - $t"
  done
  exit 1
fi

echo "::warning::All $FAILED_COUNT previously-failed test(s) passed on retry (flaky in '$TEST_MODE' mode):"
echo -e "$PASSED_ON_RETRY" | while IFS= read -r t; do
  [ -n "$t" ] && echo "  - $t"
done
exit 0
