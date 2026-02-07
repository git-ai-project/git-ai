#!/usr/bin/env python3
"""
Compare git test results between standard git and git-ai.

This script parses prove output files and identifies tests that fail
only when git-ai is enabled (regressions caused by git-ai).

Usage:
    python3 compare_results.py standard_test_results.txt gitai_test_results.txt
"""

import re
import sys
from typing import Dict, Set


def parse_test_summary(filename: str) -> Dict[str, Set[int]]:
    """Parse the Test Summary Report section from prove output."""
    try:
        with open(filename, 'r') as f:
            content = f.read()
    except FileNotFoundError:
        print(f"Warning: File not found: {filename}")
        return {}

    failures: Dict[str, Set[int]] = {}
    m = re.search(r"(?ms)^Test Summary Report\n[-]+\n(.*)$", content)
    if not m:
        return failures

    summary = m.group(1)
    lines = summary.splitlines()
    current_test = None

    for line in lines:
        # Match test file header
        header = re.match(r"^(t\d{4}-.+?\.sh)\s+\(Wstat:", line.strip())
        if header:
            current_test = header.group(1)
            failures[current_test] = set()
            continue

        # Match failed tests line
        if current_test:
            failed = re.match(r"^\s*Failed tests?:\s*(.+)$", line)
            if failed:
                nums_str = failed.group(1)
                # Parse numbers and ranges
                for tok in re.split(r"[,\s]+", nums_str.strip()):
                    tok = re.sub(r"[^\d\-]", "", tok)
                    if not tok:
                        continue
                    if "-" in tok:
                        parts = tok.split("-", 1)
                        if parts[0].isdigit() and parts[1].isdigit():
                            lo, hi = int(parts[0]), int(parts[1])
                            failures[current_test].update(range(lo, hi + 1))
                    elif tok.isdigit():
                        failures[current_test].add(int(tok))

    return failures


def condense_indices(nums: Set[int]) -> str:
    """Turn {1,2,3,5,8,9,10} into '1-3, 5, 8-10'."""
    if not nums:
        return ""
    sorted_nums = sorted(nums)
    ranges = []
    start = prev = sorted_nums[0]
    for n in sorted_nums[1:]:
        if n == prev + 1:
            prev = n
        else:
            ranges.append(f"{start}-{prev}" if start != prev else f"{start}")
            start = prev = n
    ranges.append(f"{start}-{prev}" if start != prev else f"{start}")
    return ", ".join(ranges)


def main():
    if len(sys.argv) != 3:
        print("Usage: python3 compare_results.py <standard_results> <gitai_results>")
        sys.exit(1)

    standard_file = sys.argv[1]
    gitai_file = sys.argv[2]

    # Parse both result files
    standard_failures = parse_test_summary(standard_file)
    gitai_failures = parse_test_summary(gitai_file)

    # Find tests that fail ONLY with git-ai (not standard git)
    gitai_only_failures: Dict[str, Set[int]] = {}
    # Tests that completely failed (crash, timeout, no TAP output) only under git-ai
    gitai_only_complete_failures = []
    for test, indices in gitai_failures.items():
        std_indices = standard_failures.get(test, set())
        if not indices and test not in standard_failures:
            # Test completely failed under git-ai (no subtest-level info) but
            # passed under standard git — this is a regression.
            gitai_only_complete_failures.append(test)
        else:
            only_gitai = indices - std_indices
            if only_gitai:
                gitai_only_failures[test] = only_gitai

    has_regressions = bool(gitai_only_failures) or bool(gitai_only_complete_failures)

    print("=== Git Compatibility Test Analysis ===")
    print()
    print(f"Standard git failures: {sum(len(v) for v in standard_failures.values())} subtests in {len(standard_failures)} tests")
    print(f"Git-AI failures: {sum(len(v) for v in gitai_failures.values())} subtests in {len(gitai_failures)} tests")
    print()

    if has_regressions:
        print("❌ REGRESSIONS DETECTED: Tests that fail with git-ai but NOT with standard git:")
        print()
        for test in sorted(gitai_only_complete_failures):
            print(f"  {test}: COMPLETE FAILURE (crash, timeout, or no TAP output)")
        for test in sorted(gitai_only_failures.keys()):
            indices = gitai_only_failures[test]
            print(f"  {test}: subtests {condense_indices(indices)}")
        print()
        print("These failures are caused by git-ai and must be investigated.")
        print()
        print("To reproduce locally:")
        print("  1. Clone the git repository: git clone https://github.com/git/git.git")
        print("  2. Build git-ai: cargo build --release")
        print("  3. Set up gitwrap: mkdir -p ~/.git-ai-test/gitwrap/bin && ln -sf $(pwd)/target/release/git-ai ~/.git-ai-test/gitwrap/bin/git")
        print("  4. Run the failing test: cd git/t && GIT_TEST_INSTALLED=~/.git-ai-test/gitwrap/bin ./<test>.sh -v")
        sys.exit(1)
    else:
        print("✅ No regressions detected!")
        print("All test failures (if any) also occur with standard git.")
        sys.exit(0)


if __name__ == "__main__":
    main()
