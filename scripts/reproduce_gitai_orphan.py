#!/usr/bin/env python3
"""
Reproduces the git-ai INITIAL attribution orphaning bug during rebase.

Uses real git-ai and Graphite (gt) commands to demonstrate that uncommitted
AI attributions (stored in INITIAL files) are lost when a rebase rewrites
commit SHAs. In a real workflow, rebases are triggered by:
  - gt sync     (fetches trunk + rebases feature branches)
  - gt restack  (rebases branches to maintain stack ordering)
  - git rebase  (direct rebase)

This script uses `gt create` and `gt modify` for commits (exactly as in a
real Graphite workflow), and `git rebase main` for the rebase step (which is
the underlying operation that gt sync/restack invoke).

Prerequisites:
  - git-ai >= 1.1.3 installed and in PATH (as git proxy or standalone)
  - gt (Graphite CLI) installed and in PATH
  - Python 3.8+

Usage:
  python3 scripts/reproduce_gitai_orphan.py
"""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


SEPARATOR: str = "─" * 70


def run(cmd: str, cwd: str, check: bool = False, label: str = "") -> subprocess.CompletedProcess[str]:
    """Run a shell command, print it and its output, return the result."""
    if label:
        print(f"  # {label}")
    print(f"  $ {cmd}")
    result: subprocess.CompletedProcess[str] = subprocess.run(
        cmd, shell=True, cwd=cwd, capture_output=True, text=True
    )
    if result.stdout.strip():
        for line in result.stdout.strip().split("\n"):
            print(f"    {line}")
    if result.stderr.strip():
        for line in result.stderr.strip().split("\n"):
            if any(skip in line.lower() for skip in [
                "switched to", "rebasing", "successfully rebased",
                "checkpoint completed", "changed"
            ]):
                print(f"    {line}")
            else:
                print(f"    [stderr] {line}")
    if result.returncode != 0 and check:
        print(f"    *** COMMAND FAILED (exit {result.returncode}) ***")
        sys.exit(1)
    return result


def head_sha(cwd: str) -> str:
    """Return full HEAD SHA."""
    r: subprocess.CompletedProcess[str] = subprocess.run(
        "git rev-parse HEAD", shell=True, cwd=cwd, capture_output=True, text=True
    )
    return r.stdout.strip()


def short(sha: str) -> str:
    """Shorten SHA to 8 chars."""
    return sha[:8]


def pct(n: int, total: int) -> str:
    """Format a percentage string, handling zero division."""
    if total == 0:
        return "N/A"
    return f"{n / total * 100:.0f}%"


def measure_attribution(cwd: str, label: str, expected: str = "") -> dict[str, Any]:
    """Measure and display git-ai's attribution for committed and uncommitted work."""
    print(f"\n  {'─' * 60}")
    print(f"  ATTRIBUTION: {label}")
    if expected:
        print(f"  Expected: {expected}")
    print(f"  {'─' * 60}")

    result: dict[str, Any] = {
        "committed": {}, "uncommitted": {}, "notes_exist": False
    }

    # Committed attribution (git-ai stats on HEAD)
    stats_r: subprocess.CompletedProcess[str] = subprocess.run(
        "git-ai stats --json", shell=True, cwd=cwd,
        capture_output=True, text=True
    )
    if stats_r.returncode == 0 and stats_r.stdout.strip():
        try:
            cs: dict[str, Any] = json.loads(stats_r.stdout)
            ai_accepted: int = cs.get("ai_accepted", 0)
            human_add: int = cs.get("human_additions", 0)
            total_add: int = cs.get("git_diff_added_lines", 0)
            breakdown: dict[str, Any] = cs.get("tool_model_breakdown", {})
            result["committed"] = cs
            result["notes_exist"] = ai_accepted > 0 or human_add > 0

            print(f"  Committed (HEAD git note):")
            print(f"    AI accepted:     {ai_accepted:3d} lines ({pct(ai_accepted, total_add)} of {total_add} additions)")
            print(f"    Human additions: {human_add:3d} lines ({pct(human_add, total_add)} of {total_add} additions)")
            if breakdown:
                for tool, data in breakdown.items():
                    print(f"    Tool: {tool} -> {data.get('ai_accepted', 0)} accepted")
        except json.JSONDecodeError:
            print(f"  Committed: (parse error)")

    # Uncommitted attribution (git-ai status)
    status_r: subprocess.CompletedProcess[str] = subprocess.run(
        "git-ai status --json", shell=True, cwd=cwd,
        capture_output=True, text=True
    )
    if status_r.returncode == 0 and status_r.stdout.strip():
        try:
            st: dict[str, Any] = json.loads(status_r.stdout)
            stats: dict[str, Any] = st.get("stats", {})
            cps: list[Any] = st.get("checkpoints", [])
            ai_add: int = stats.get("ai_additions", 0)
            ai_accepted: int = stats.get("ai_accepted", 0)
            human_add: int = stats.get("human_additions", 0)
            total_ai: int = stats.get("total_ai_additions", 0)
            result["uncommitted"] = stats

            print(f"  Uncommitted (working log + INITIAL):")
            print(f"    AI additions:    {ai_add:3d} lines (tracked, will be attributed to AI on commit)")
            print(f"    Human additions: {human_add:3d} lines")
            print(f"    Checkpoints:     {len(cps)}")
            if cps:
                for cp in cps:
                    tool: str = cp.get("tool_model", "unknown")
                    adds: int = cp.get("additions", 0)
                    ago: str = cp.get("time_ago", "")
                    is_human: bool = cp.get("is_human", False)
                    kind: str = "Human" if is_human else "AI"
                    print(f"      [{kind}] {tool}: +{adds} lines ({ago})")
        except json.JSONDecodeError:
            print(f"  Uncommitted: (parse error)")

    print()
    return result


def dump_working_logs(cwd: str) -> None:
    """Dump the on-disk state of working log directories."""
    current: str = head_sha(cwd)
    wl_dir: Path = Path(cwd) / ".git" / "ai" / "working_logs"
    if not wl_dir.exists():
        print("  Working Logs: (directory does not exist)\n")
        return

    dirs: list[Path] = sorted(
        [d for d in wl_dir.iterdir() if d.is_dir()],
        key=lambda p: p.stat().st_mtime
    )
    print(f"  Working Logs ({len(dirs)} director{'y' if len(dirs) == 1 else 'ies'}):")
    if not dirs:
        print("    (empty)\n")
        return

    for sha_dir in dirs:
        sha: str = sha_dir.name
        is_head: bool = sha == current

        branch_check: subprocess.CompletedProcess[str] = subprocess.run(
            f"git branch --contains {sha} 2>/dev/null", shell=True,
            cwd=cwd, capture_output=True, text=True
        )
        branches: list[str] = [
            b.strip().lstrip("* ")
            for b in branch_check.stdout.strip().split("\n")
            if b.strip()
        ]

        cat_check: subprocess.CompletedProcess[str] = subprocess.run(
            f"git cat-file -t {sha}", shell=True, cwd=cwd,
            capture_output=True, text=True
        )
        sha_exists: bool = cat_check.returncode == 0

        if not sha_exists:
            status = "SHA GONE"
        elif not branches:
            status = "ORPHANED"
        else:
            status = f"LIVE ({', '.join(branches)})"

        marker: str = " <-- HEAD" if is_head else ""
        print(f"\n    [{short(sha)}] {status}{marker}")

        initial_file: Path = sha_dir / "INITIAL"
        if initial_file.exists():
            data: dict[str, Any] = json.loads(initial_file.read_text())
            files_data: dict[str, list[Any]] = data.get("files", {})
            for fpath, attrs in files_data.items():
                ranges: str = ", ".join(
                    f"L{a['start_line']}-{a['end_line']}" for a in attrs
                )
                print(f"      INITIAL: {fpath} -> {ranges}")
        else:
            print(f"      INITIAL: (none)")

        cp_file: Path = sha_dir / "checkpoints.jsonl"
        if cp_file.exists() and cp_file.stat().st_size > 0:
            lines: list[str] = [l for l in cp_file.read_text().strip().split("\n") if l]
            ai_n: int = sum(1 for l in lines if '"AiAgent"' in l or '"AiTab"' in l)
            human_n: int = sum(1 for l in lines if '"Human"' in l)
            print(f"      Checkpoints: {len(lines)} ({ai_n} AI, {human_n} Human)")
        else:
            print(f"      Checkpoints: (none)")

    print()


def require_tool(name: str) -> str:
    """Verify a CLI tool exists and return its version string."""
    r: subprocess.CompletedProcess[str] = subprocess.run(
        f"{name} --version",
        shell=True, capture_output=True, text=True
    )
    if r.returncode != 0:
        print(f"ERROR: '{name}' not found in PATH.")
        sys.exit(1)
    version: str = r.stdout.strip().split("\n")[0]
    return version


def main() -> None:
    print("=" * 70)
    print("  git-ai INITIAL Attribution Orphaning -- Reproduction")
    print("  Using real git-ai + Graphite (gt) commands")
    print("=" * 70)

    gitai_ver: str = require_tool("git-ai")
    gt_ver: str = require_tool("gt")
    print(f"  git-ai: {gitai_ver}")
    print(f"  gt:     {gt_ver}")

    tmp_dir: str = tempfile.mkdtemp(prefix="gitai-orphan-repro-")
    repo_dir: str = os.path.join(tmp_dir, "test-repo")
    os.makedirs(repo_dir)
    print(f"  Repo:   {repo_dir}\n")

    try:
        # ──────────────────────────────────────────────────────
        # PHASE 1: Initialize repo with Graphite
        # ──────────────────────────────────────────────────────
        print(f"\n{SEPARATOR}")
        print("PHASE 1: Initialize repo with git + Graphite")
        print(SEPARATOR)

        run("git init -b main", repo_dir, check=True)
        run('git config user.email "repro@git-ai.dev"', repo_dir)
        run('git config user.name "Repro Script"', repo_dir)

        Path(repo_dir, "app.py").write_text(
            "def main():\n"
            "    print('hello')\n"
            "\n"
            "if __name__ == '__main__':\n"
            "    main()\n"
        )
        run("git add .", repo_dir, check=True)
        run('git commit -m "Initial commit"', repo_dir, check=True)

        run("gt init --trunk main --no-interactive", repo_dir, check=True,
            label="Tell Graphite that 'main' is our trunk")

        measure_attribution(repo_dir, "Baseline (initial commit, no AI)",
                            expected="0% AI -- nothing AI-authored yet")

        # ──────────────────────────────────────────────────────
        # PHASE 2: Create feature branch via gt, AI edits TWO files
        # ──────────────────────────────────────────────────────
        print(f"{SEPARATOR}")
        print("PHASE 2: AI edits two files, commit only one via `gt create`")
        print(SEPARATOR)
        print("  Scenario: Cursor AI edits app.py AND utils.py.")
        print("  We only commit app.py via `gt create`.")
        print("  utils.py's AI attribution should go to INITIAL.\n")

        Path(repo_dir, "app.py").write_text(
            "import logging\n"
            "\n"
            "logger = logging.getLogger(__name__)\n"
            "\n"
            "def main():\n"
            "    logger.info('Starting')\n"
            "    result = compute()\n"
            "    logger.info(f'Result: {result}')\n"
            "\n"
            "def compute():\n"
            "    return 42\n"
            "\n"
            "if __name__ == '__main__':\n"
            "    main()\n"
        )
        Path(repo_dir, "utils.py").write_text(
            "def helper_one():\n"
            "    return 'one'\n"
            "\n"
            "def helper_two():\n"
            "    return 'two'\n"
            "\n"
            "def helper_three():\n"
            "    return 'three'\n"
        )

        run("git-ai checkpoint mock_ai app.py utils.py", repo_dir,
            label="Record AI authorship for both files via git-ai checkpoint")

        measure_attribution(repo_dir, "After AI edits, before commit",
                            expected="18 uncommitted AI additions (app.py + utils.py)")

        run("git add app.py", repo_dir,
            label="Stage only app.py (utils.py intentionally left unstaged)")
        run('gt create feature/ai-work -m "Add logging and compute" --no-interactive', repo_dir, check=True,
            label="gt create -- creates branch + commit (triggers git-ai post-commit hook)")

        feature_branch: str = subprocess.run(
            "git branch --show-current", shell=True, cwd=repo_dir,
            capture_output=True, text=True
        ).stdout.strip()
        print(f"  Feature branch: {feature_branch}")

        measure_attribution(repo_dir, "After gt create (app.py committed, utils.py uncommitted)",
                            expected="Committed: 10/10 AI (app.py). Uncommitted: 8 AI (utils.py in INITIAL)")
        dump_working_logs(repo_dir)

        # ──────────────────────────────────────────────────────
        # PHASE 3: Simulate more AI edits + gt modify (amend)
        # ──────────────────────────────────────────────────────
        print(f"{SEPARATOR}")
        print("PHASE 3: More AI edits + `gt modify` (amend)")
        print(SEPARATOR)
        print("  Tests that INITIAL survives an amend cycle.\n")

        Path(repo_dir, "app.py").write_text(
            "import logging\n"
            "\n"
            "logger = logging.getLogger(__name__)\n"
            "\n"
            "def main():\n"
            "    logger.info('Starting app v2')\n"
            "    result = compute()\n"
            "    logger.info(f'Result: {result}')\n"
            "    return result\n"
            "\n"
            "def compute():\n"
            "    return 42 * 2\n"
            "\n"
            "if __name__ == '__main__':\n"
            "    main()\n"
        )

        run("git-ai checkpoint mock_ai app.py", repo_dir,
            label="Record AI authorship for app.py v2 edits")
        run("git add app.py", repo_dir)
        run('gt modify -m "Add logging and compute v2" --no-interactive', repo_dir,
            label="gt modify -- amends commit (triggers git-ai amend rewrite hook)")

        measure_attribution(repo_dir, "After gt modify (amend)",
                            expected="Committed: 11/11 AI (app.py). Uncommitted: 8 AI (utils.py still in INITIAL)")
        dump_working_logs(repo_dir)

        # ──────────────────────────────────────────────────────
        # PHASE 4: Advance main (simulate upstream changes)
        # ──────────────────────────────────────────────────────
        print(f"{SEPARATOR}")
        print("PHASE 4: Advance main (simulates upstream changes)")
        print(SEPARATOR)

        run("git checkout main", repo_dir)
        Path(repo_dir, "README.md").write_text("# Test Project\nVersion 1\n")
        run("git add README.md", repo_dir)
        run('git commit -m "Add README"', repo_dir, check=True)
        run(f"git checkout {feature_branch}", repo_dir)

        pre_rebase_sha: str = head_sha(repo_dir)

        measure_attribution(repo_dir, "Feature branch before rebase",
                            expected="Same as Phase 3 -- 11/11 committed AI, 8 uncommitted AI")
        dump_working_logs(repo_dir)

        # ──────────────────────────────────────────────────────
        # PHASE 5: git rebase main -- THE BUG
        # ──────────────────────────────────────────────────────
        print(f"{SEPARATOR}")
        print("PHASE 5: `git rebase main` -- THIS TRIGGERS THE BUG")
        print(SEPARATOR)
        print("  In a real workflow: `gt sync` or `gt restack` triggers this rebase.")
        print("  git-ai intercepts via rewrite_authorship_after_rebase_v2.")
        print("  Notes (committed attribution) are migrated. INITIAL is NOT.\n")

        print(f"  Pre-rebase HEAD: {short(pre_rebase_sha)}")

        run("git rebase main", repo_dir,
            label="git rebase main -- same operation that gt sync / gt restack perform")

        post_rebase_sha: str = head_sha(repo_dir)
        sha_changed: bool = pre_rebase_sha != post_rebase_sha
        print(f"  Post-rebase HEAD: {short(post_rebase_sha)}")
        print(f"  SHA changed: {sha_changed} ({short(pre_rebase_sha)} -> {short(post_rebase_sha)})")

        measure_attribution(repo_dir, "After rebase -- uncommitted AI should be LOST",
                            expected="Committed: 11/11 AI (notes migrated OK). "
                                     "Uncommitted: SHOULD be 8 AI (utils.py) but will show 0")
        dump_working_logs(repo_dir)

        # ──────────────────────────────────────────────────────
        # PHASE 6: Commit utils.py -- THE IMPACT
        # ──────────────────────────────────────────────────────
        print(f"{SEPARATOR}")
        print("PHASE 6: Commit utils.py -- SHOWS THE ATTRIBUTION LOSS")
        print(SEPARATOR)
        print("  utils.py was 100% AI-authored. But because INITIAL was orphaned,")
        print("  git-ai no longer knows those lines came from AI.\n")

        run("git add utils.py", repo_dir)
        run('gt modify -m "Add logging, compute, and utils" --no-interactive', repo_dir,
            label="gt modify -- commits utils.py (amends current commit)")

        measure_attribution(repo_dir, "After committing utils.py (the money shot)",
                            expected="SHOULD be 19/19 AI (app.py 11 + utils.py 8). "
                                     "ACTUAL will show utils.py as HUMAN because INITIAL was lost")
        dump_working_logs(repo_dir)

        # ──────────────────────────────────────────────────────
        # PHASE 7: Diagnosis
        # ──────────────────────────────────────────────────────
        print(f"{SEPARATOR}")
        print("PHASE 7: Diagnosis")
        print(SEPARATOR)

        old_wl: Path = Path(repo_dir) / ".git" / "ai" / "working_logs" / pre_rebase_sha
        old_initial: Path = old_wl / "INITIAL"

        print(f"  Old working log ({short(pre_rebase_sha)}): {'EXISTS' if old_wl.exists() else 'GONE'}")
        print(f"  Old INITIAL:                  {'EXISTS' if old_initial.exists() else 'GONE'}")

        if old_initial.exists():
            lost: dict[str, Any] = json.loads(old_initial.read_text()).get("files", {})
            total_lines: int = sum(
                sum(a["end_line"] - a["start_line"] + 1 for a in attrs)
                for attrs in lost.values()
            )
            print(f"\n  ╔══════════════════════════════════════════════════════════════╗")
            print(f"  ║  BUG CONFIRMED: INITIAL attributions ORPHANED by rebase     ║")
            print(f"  ╚══════════════════════════════════════════════════════════════╝")
            print(f"\n  {len(lost)} file(s), {total_lines} AI-attributed line(s) stranded on old SHA:")
            for fpath, attrs in lost.items():
                for a in attrs:
                    print(f"    {fpath}: lines {a['start_line']}-{a['end_line']} [author: {a['author_id']}]")

        # Final attribution comparison
        final_stats_r: subprocess.CompletedProcess[str] = subprocess.run(
            "git-ai stats --json", shell=True, cwd=repo_dir,
            capture_output=True, text=True
        )
        if final_stats_r.returncode == 0:
            try:
                fs: dict[str, Any] = json.loads(final_stats_r.stdout)
                ai: int = fs.get("ai_accepted", 0)
                human: int = fs.get("human_additions", 0)
                total: int = fs.get("git_diff_added_lines", 0)

                print(f"\n  Final attribution for HEAD commit:")
                print(f"    Total additions: {total} lines")
                print(f"    AI accepted:     {ai} lines ({pct(ai, total)})")
                print(f"    Human:           {human} lines ({pct(human, total)})")
                print(f"\n    Expected:        19/19 AI = 100% AI")
                print(f"    Actual:          {ai}/{total} AI = {pct(ai, total)} AI")
                if human > 0:
                    print(f"\n    {human} lines INCORRECTLY attributed to human author.")
                    print(f"    These were the utils.py lines whose INITIAL was orphaned.")
            except json.JSONDecodeError:
                pass

        # ──────────────────────────────────────────────────────
        # Summary
        # ──────────────────────────────────────────────────────
        print(f"\n{SEPARATOR}")
        print("SUMMARY")
        print(SEPARATOR)
        print("""
  Root cause: rewrite_authorship_after_rebase_v2() in rebase_authorship.rs
  correctly rewrites git notes (committed attribution) for rebased commits,
  but does NOT migrate the working log directory or INITIAL file from
  the original HEAD SHA to the new HEAD SHA.

  Affected operations:
    - gt sync       (rebases feature branches onto updated trunk)
    - gt restack    (rebases branches to maintain stack ordering)
    - git rebase    (direct rebase)

  NOT affected (these already handle INITIAL correctly):
    - gt create     (git commit    -> post_commit.rs writes INITIAL)
    - gt modify     (git commit --amend -> rewrite_authorship_after_commit_amend writes INITIAL)
    - git reset     (reconstruct_working_log_after_reset writes INITIAL)

  Impact: Any AI-attributed lines tracked in INITIAL (lines AI wrote but
  the developer hasn't committed yet) are silently lost on every rebase.
  When eventually committed, they are counted as human-written.
""")
        print(f"  Test repo: {repo_dir}")
        print(f"  Inspect:")
        print(f"    cd {repo_dir}")
        print(f"    find .git/ai -type f | sort")
        print(f"    git log --oneline --all --graph")
        print(f"    git-ai stats --json")

    except Exception as e:
        print(f"\n  ERROR: {e}")
        import traceback
        traceback.print_exc()
        print(f"\n  Test repo: {repo_dir}")
        sys.exit(1)


if __name__ == "__main__":
    main()
