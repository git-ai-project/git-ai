#!/bin/bash
# Update coverage threshold based on current coverage rounded down to nearest 5%
#
# Usage: ./scripts/update-coverage-threshold.sh
#
# This script:
# 1. Runs coverage analysis
# 2. Calculates current coverage percentage
# 3. Rounds down to nearest multiple of 5
# 4. Updates the threshold in .github/workflows/coverage.yml

set -e

echo "Running coverage analysis..."
cargo llvm-cov test --ignore-filename-regex='tests/.*|benches/.*|examples/.*' --lcov --output-path /tmp/coverage-temp.lcov -- --skip performance_regression

echo "Calculating coverage percentage..."
COVERAGE=$(awk -F: '
/^DA:/ {
    total++;
    split($2, parts, ",");
    if (parts[2] > 0) covered++;
}
END {
    if (total > 0) {
        pct = (covered / total) * 100;
        printf "%.2f", pct;
    }
}' /tmp/coverage-temp.lcov)

echo "Current coverage: ${COVERAGE}%"

# Round down to nearest 5
THRESHOLD=$(echo "$COVERAGE" | awk '{printf "%d", int($1/5)*5}')
echo "Threshold (rounded down to nearest 5): ${THRESHOLD}%"

# Update coverage.yml
sed -i.bak "s/COVERAGE_THRESHOLD: [0-9]*/COVERAGE_THRESHOLD: ${THRESHOLD}/" .github/workflows/coverage.yml

echo "Updated .github/workflows/coverage.yml with threshold: ${THRESHOLD}%"
echo "Please review the changes and commit if appropriate."

# Clean up
rm -f /tmp/coverage-temp.lcov .github/workflows/coverage.yml.bak
