#!/usr/bin/env python3
"""
Run a curated subset of the Git test suite against git-ai.

This harness clones the Git repository (or reuses a local clone), runs the
selected tests with GIT_TEST_INSTALLED pointing at a git-ai wrapper, and
fails if any new test failures appear outside the whitelist.
"""

from __future__ import annotations

import argparse
import csv
import os
import re
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Dict, List, Set, Tuple

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_TESTS_FILE = REPO_ROOT / "tests" / "git-compat" / "core-tests.txt"
DEFAULT_WHITELIST = REPO_ROOT / "tests" / "git-compat" / "whitelist.csv"
DEFAULT_GIT_URL = "https://github.com/git/git.git"
DEFAULT_CLONE_DIR = Path("/tmp/git-core-tests")


def read_tests_list(path: Path) -> List[str]:
    tests: List[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        tests.append(stripped)
    if not tests:
        raise ValueError(f"No tests found in {path}")
    return tests


def ensure_git_clone(clone_dir: Path, clone_url: str) -> None:
    if clone_dir.exists():
        return
    clone_dir.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "clone", "--depth", "1", clone_url, str(clone_dir)],
        check=True,
    )


def ensure_git_build(clone_dir: Path, jobs: int) -> None:
    build_options = clone_dir / "GIT-BUILD-OPTIONS"
    if build_options.exists():
        return
    subprocess.run(
        [
            "make",
            "-C",
            str(clone_dir),
            f"-j{jobs}",
            "NO_CURL=YesPlease",
            "NO_GETTEXT=YesPlease",
        ],
        check=True,
    )


def run_prove(git_tests_dir: Path, tests: List[str], git_installed: Path, jobs: int) -> Tuple[int, str]:
    env = os.environ.copy()
    env["GIT_TEST_INSTALLED"] = str(git_installed)
    env.setdefault("GIT_TEST_DEFAULT_HASH", "sha1")

    cmd = ["prove", f"-j{jobs}"] + tests
    proc = subprocess.Popen(
        cmd,
        cwd=git_tests_dir,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )

    combined_lines: List[str] = []
    assert proc.stdout is not None
    for line in proc.stdout:
        sys.stdout.write(line)
        sys.stdout.flush()
        combined_lines.append(line)

    proc.wait()
    return proc.returncode, "".join(combined_lines)


def extract_summary_section(prove_output: str) -> str:
    m = re.search(r"(?ms)^Test Summary Report\n[-]+\n(.*)$", prove_output)
    return m.group(1).strip() if m else ""


def parse_failed_indices_list(s: str) -> Set[int]:
    out: Set[int] = set()
    for tok in re.split(r"[,\s]+", s.strip()):
        if not tok:
            continue
        tok = tok.strip().rstrip(".")
        tok = re.sub(r"[^\d\-]", "", tok)
        if not tok:
            continue
        if "-" in tok:
            a, b = tok.split("-", 1)
            if a.isdigit() and b.isdigit():
                lo, hi = int(a), int(b)
                if hi < lo:
                    lo, hi = hi, lo
                out.update(range(lo, hi + 1))
        elif tok.isdigit():
            out.add(int(tok))
    return out


def parse_failures(summary_text: str) -> Dict[str, List[int]]:
    failures: Dict[str, Set[int]] = {}
    lines = summary_text.splitlines()
    i = 0
    current = None

    header_re = re.compile(r"^(t\d{4}-.+?\.sh)\s+\(Wstat:.*\)$")
    failed_re = re.compile(r"^\s*Failed tests?:\s*(.+)$")

    while i < len(lines):
        line = lines[i].rstrip("\n")
        header = header_re.match(line.strip())
        if header:
            current = header.group(1)
            failures.setdefault(current, set())
            i += 1
            continue

        if current:
            m = failed_re.match(line)
            if m:
                first = m.group(1).strip()
                j = i + 1
                cont_parts: List[str] = []
                while j < len(lines):
                    nxt = lines[j]
                    if re.match(r"^\s{2,}[\d,\-\s\.]+$", nxt):
                        cont_parts.append(nxt.strip())
                        j += 1
                    else:
                        break
                full = ", ".join([first] + cont_parts) if cont_parts else first
                failures[current].update(parse_failed_indices_list(full))
                i = j
                continue

        i += 1

    return {k: sorted(v) for k, v in failures.items()}


def parse_summary_issues(summary_text: str) -> Dict[str, List[str]]:
    issues: Dict[str, List[str]] = {}
    lines = summary_text.splitlines()
    i = 0
    current = None
    header_re = re.compile(r"^(t\d{4}-.+?\.sh)\s+\(Wstat:.*\)$")
    issue_re = re.compile(r"^\s*(Parse errors):\s*(.+)$")

    while i < len(lines):
        line = lines[i].rstrip("\n")
        header = header_re.match(line.strip())
        if header:
            current = header.group(1)
            issues.setdefault(current, [])
            i += 1
            continue
        if current:
            match = issue_re.match(line)
            if match:
                label, detail = match.groups()
                issues[current].append(f"{label}: {detail}")
        i += 1
    return {k: v for k, v in issues.items() if v}


def load_whitelist(path: Path) -> Dict[str, Set[int]]:
    whitelist: Dict[str, Set[int]] = {}
    if not path.exists():
        return whitelist
    with path.open("r", encoding="utf-8", newline="") as f:
        reader = csv.DictReader(f)
        for row in reader:
            file_key = (row.get("file") or "").strip().strip('"')
            tests_field = (row.get("test") or row.get("tests") or "").strip().strip('"')
            if not file_key or not tests_field:
                continue
            indices = parse_failed_indices_list(tests_field)
            if not indices:
                continue
            whitelist.setdefault(file_key, set()).update(indices)
    return whitelist


def apply_whitelist(failures: Dict[str, List[int]], whitelist: Dict[str, Set[int]]) -> Dict[str, List[int]]:
    if not whitelist:
        return failures
    filtered: Dict[str, List[int]] = {}
    for test_name, indices in failures.items():
        wl = whitelist.get(test_name)
        if wl:
            remaining = [i for i in indices if i not in wl]
        else:
            remaining = list(indices)
        if remaining:
            filtered[test_name] = remaining
    return filtered


def format_failures(failures: Dict[str, List[int]]) -> str:
    lines = []
    for test_name in sorted(failures.keys()):
        nums = ", ".join(str(i) for i in failures[test_name])
        lines.append(f"  - {test_name}: {nums}")
    return "\n".join(lines)


def format_summary_issues(issues: Dict[str, List[str]]) -> str:
    lines = []
    for test_name in sorted(issues.keys()):
        for issue in issues[test_name]:
            lines.append(f"  - {test_name}: {issue}")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Run core Git tests against git-ai.")
    parser.add_argument("--tests-file", type=Path, default=DEFAULT_TESTS_FILE)
    parser.add_argument("--whitelist", type=Path, default=DEFAULT_WHITELIST)
    parser.add_argument("--git-url", default=DEFAULT_GIT_URL)
    parser.add_argument("--clone-dir", type=Path, default=DEFAULT_CLONE_DIR)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--git-ai-bin", type=Path, default=REPO_ROOT / "target" / "release" / "git-ai")
    args = parser.parse_args()

    tests = read_tests_list(args.tests_file)

    if not args.git_ai_bin.exists():
        raise FileNotFoundError(
            f"git-ai binary not found at {args.git_ai_bin}. Build it with `cargo build --release`."
        )

    ensure_git_clone(args.clone_dir, args.git_url)
    ensure_git_build(args.clone_dir, args.jobs)
    git_tests_dir = args.clone_dir / "t"

    if not git_tests_dir.exists():
        raise FileNotFoundError(f"Git tests directory not found at {git_tests_dir}")

    whitelist = load_whitelist(args.whitelist)

    with tempfile.TemporaryDirectory() as tmpdir:
        wrapper_dir = Path(tmpdir)
        (wrapper_dir / "git").symlink_to(args.git_ai_bin)
        (wrapper_dir / "git-ai").symlink_to(args.git_ai_bin)

        cmd_preview = " ".join(shlex.quote(t) for t in tests)
        print(f"[+] Running core Git tests with: prove -j{args.jobs} {cmd_preview}")
        print(f"[+] GIT_TEST_INSTALLED={wrapper_dir}")

        exit_code, output = run_prove(git_tests_dir, tests, wrapper_dir, args.jobs)

    summary = extract_summary_section(output)
    failures = parse_failures(summary) if summary else {}
    unexpected = apply_whitelist(failures, whitelist)
    summary_issues = parse_summary_issues(summary) if summary else {}

    print("\n[+] Test Summary Report")
    print(summary or "(no failures)")

    if summary_issues:
        print("\n[!] Test harness errors detected:")
        print(format_summary_issues(summary_issues))
        return 1

    if unexpected:
        print("\n[!] Unexpected failures detected (not in whitelist):")
        print(format_failures(unexpected))
        print("\nUpdate tests/git-compat/whitelist.csv to acknowledge known failures.")
        return 1

    if exit_code != 0 and not failures:
        print("\n[!] prove exited non-zero but no failures were parsed. Please investigate the output above.")
        return exit_code

    print("\n[+] All core tests passed or are whitelisted.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
