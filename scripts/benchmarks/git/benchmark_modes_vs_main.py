#!/usr/bin/env python3
"""
Benchmark git-ai execution modes against main-branch wrapper mode.

Compares:
1) main(wrapper)
2) current(wrapper)
3) current(hooks)
4) current(wrapper+hooks)

The harness builds binaries for both branches, runs a scenario matrix that
covers basic and complex Git operations, and emits:
- raw CSV timings
- machine-readable JSON summary
- Markdown report
"""

from __future__ import annotations

import argparse
import csv
import dataclasses
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Callable


MANAGED_HOOK_NAMES = [
    "pre-commit",
    "prepare-commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "post-rewrite",
    "reference-transaction",
]


class BenchmarkError(RuntimeError):
    pass


@dataclasses.dataclass(frozen=True)
class Variant:
    key: str
    label: str
    binary: Path
    mode: str  # wrapper | hooks | both


@dataclasses.dataclass(frozen=True)
class Scenario:
    key: str
    description: str
    complexity: str  # basic | complex
    setup: Callable[["VariantRunner", Path], None]
    measure: Callable[["VariantRunner", Path, int], None]


@dataclasses.dataclass
class RunResult:
    scenario: str
    complexity: str
    variant: str
    run_index: int
    duration_ms: float


@dataclasses.dataclass(frozen=True)
class MarginCheckResult:
    scenario: str
    variant: str
    baseline_ms: float
    median_ms: float
    allowed_ms: float
    slowdown_pct: float
    passed: bool


def now_iso_utc() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def create_link_or_copy(target: Path, link_path: Path) -> None:
    if link_path.exists() or link_path.is_symlink():
        if link_path.is_dir() and not link_path.is_symlink():
            shutil.rmtree(link_path)
        else:
            link_path.unlink()
    link_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        link_path.symlink_to(target)
    except OSError:
        shutil.copy2(target, link_path)


def write_seed_file(path: Path, seed: int, lines: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as fh:
        for i in range(1, lines + 1):
            payload = (seed * 1315423911 + i * 2654435761) & 0xFFFFFFFF
            fh.write(f"seed={seed:08d} line={i:04d} payload={payload:08x}\n")


def append_line(path: Path, line: str) -> None:
    with path.open("a", encoding="utf-8") as fh:
        fh.write(f"{line}\n")


def ignore_transient_git_lockfiles(_src: str, names: list[str]) -> set[str]:
    return {name for name in names if name.endswith(".lock")}


def run_cmd(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout_s: int = 900,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout_s,
    )
    if proc.returncode != 0:
        raise BenchmarkError(
            "Command failed\n"
            f"cmd: {' '.join(cmd)}\n"
            f"cwd: {cwd}\n"
            f"exit: {proc.returncode}\n"
            f"stdout:\n{proc.stdout}\n"
            f"stderr:\n{proc.stderr}\n"
        )
    return proc


def build_release_binary(
    repo_dir: Path,
    target_dir: Path,
) -> Path:
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    run_cmd(
        ["cargo", "build", "--release", "--bin", "git-ai"],
        cwd=repo_dir,
        env=env,
        timeout_s=3600,
    )
    if os.name == "nt":
        binary = target_dir / "release" / "git-ai.exe"
    else:
        binary = target_dir / "release" / "git-ai"
    if not binary.exists():
        raise BenchmarkError(f"Expected binary not found: {binary}")
    return binary


def git_output(repo_dir: Path, args: list[str]) -> str:
    proc = run_cmd(["git", *args], cwd=repo_dir, env=dict(os.environ), timeout_s=120)
    return (proc.stdout or "").strip()


def prepare_main_worktree(repo_root: Path, main_ref: str, worktree_dir: Path) -> None:
    worktree_dir.parent.mkdir(parents=True, exist_ok=True)
    if worktree_dir.exists():
        shutil.rmtree(worktree_dir)
    run_cmd(["git", "fetch", "--quiet", "origin", "main"], cwd=repo_root, env=dict(os.environ))
    run_cmd(
        ["git", "worktree", "add", "--detach", str(worktree_dir), main_ref],
        cwd=repo_root,
        env=dict(os.environ),
    )


def remove_main_worktree(repo_root: Path, worktree_dir: Path) -> None:
    run_cmd(
        ["git", "worktree", "remove", "--force", str(worktree_dir)],
        cwd=repo_root,
        env=dict(os.environ),
    )


def resolve_real_git_binary(repo_root: Path) -> Path:
    preferred = [
        Path("/usr/bin/git"),
        Path("/opt/homebrew/bin/git"),
        Path("/usr/local/bin/git"),
        Path("/bin/git"),
    ]
    for candidate in preferred:
        if candidate.exists() and os.access(candidate, os.X_OK):
            return candidate.resolve()

    fallback = shutil.which("git")
    if not fallback:
        raise BenchmarkError("Unable to resolve system git from PATH.")

    fallback_path = Path(fallback).resolve()
    if (
        "git-ai" in fallback_path.name.lower()
        or str(repo_root / "target") in str(fallback_path)
    ):
        raise BenchmarkError(
            "Resolved `git` points to a git-ai wrapper, not the real git binary. "
            "Install git or pass a clean PATH."
        )
    return fallback_path


class VariantRunner:
    def __init__(
        self,
        variant: Variant,
        run_root: Path,
        real_git: Path,
        timeout_s: int = 900,
    ) -> None:
        self.variant = variant
        self.real_git = real_git
        self.run_root = run_root
        self.timeout_s = timeout_s

        self.home_dir = run_root / "home"
        self.bin_dir = run_root / "bin"
        self.git_wrapper = self.bin_dir / ("git.exe" if os.name == "nt" else "git")

        self.home_dir.mkdir(parents=True, exist_ok=True)
        self.bin_dir.mkdir(parents=True, exist_ok=True)

        if self.variant.mode in ("wrapper", "both"):
            create_link_or_copy(self.variant.binary, self.git_wrapper)

        self.base_env = dict(os.environ)
        self.base_env["HOME"] = str(self.home_dir)
        self.base_env["GIT_CONFIG_GLOBAL"] = str(self.home_dir / ".gitconfig")
        self.base_env["GIT_TERMINAL_PROMPT"] = "0"
        self.base_env["GIT_AI_DEBUG"] = "0"
        self.base_env["GIT_AI_DEBUG_PERFORMANCE"] = "0"
        self.base_env["PATH"] = f"{self.bin_dir}{os.pathsep}{self.base_env.get('PATH', '')}"

    def assert_repo_local_hooks(self, repo_dir: Path) -> None:
        expected_hooks_dir = (repo_dir / ".git" / "ai" / "hooks").resolve()
        hooks_path = (
            self.run_git(["config", "--local", "--get", "core.hooksPath"], cwd=repo_dir)
            .stdout.strip()
        )
        if not hooks_path:
            raise BenchmarkError("Expected local core.hooksPath to be configured, found empty")
        resolved_hooks_path = Path(hooks_path)
        if not resolved_hooks_path.is_absolute():
            resolved_hooks_path = (repo_dir / resolved_hooks_path).resolve()
        else:
            resolved_hooks_path = resolved_hooks_path.resolve()
        if resolved_hooks_path != expected_hooks_dir:
            raise BenchmarkError(
                "Expected repo-local hooks path\n"
                f"expected={expected_hooks_dir}\n"
                f"actual={resolved_hooks_path}\n"
            )

        if not expected_hooks_dir.exists():
            raise BenchmarkError(f"Managed hooks dir missing: {expected_hooks_dir}")

        installed_hooks = {
            entry.name
            for entry in expected_hooks_dir.iterdir()
            if not entry.name.startswith(".")
        }
        expected_set = set(MANAGED_HOOK_NAMES)
        missing = sorted(expected_set.difference(installed_hooks))
        extras = sorted(installed_hooks.difference(expected_set))
        if missing or extras:
            raise BenchmarkError(
                "Unexpected managed hook surface in repo-local hooks dir\n"
                f"missing={missing}\n"
                f"extras={extras}\n"
                f"path={expected_hooks_dir}\n"
            )

    def run_git(self, args: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
        if self.variant.mode in ("wrapper", "both"):
            cmd = [str(self.git_wrapper), *args]
        else:
            cmd = [str(self.real_git), *args]
        return run_cmd(cmd, cwd=cwd, env=self.base_env, timeout_s=self.timeout_s)

    def run_git_ai(self, args: list[str], cwd: Path) -> subprocess.CompletedProcess[str]:
        return run_cmd(
            [str(self.variant.binary), *args],
            cwd=cwd,
            env=self.base_env,
            timeout_s=self.timeout_s,
        )

    def init_repo(self, repo_dir: Path) -> None:
        repo_dir.mkdir(parents=True, exist_ok=True)
        init = subprocess.run(
            [str(self.real_git), "init", "-q", "-b", "main"],
            cwd=str(repo_dir),
            env=self.base_env,
            text=True,
            capture_output=True,
            check=False,
        )
        if init.returncode != 0:
            self.run_git(["init", "-q"], cwd=repo_dir)
            self.run_git(["checkout", "-q", "-b", "main"], cwd=repo_dir)

        self.run_git(["config", "user.name", "Benchmark Bot"], cwd=repo_dir)
        self.run_git(["config", "user.email", "benchmark@git-ai.local"], cwd=repo_dir)

        if self.variant.mode in ("hooks", "both"):
            self.run_git_ai(["git-hooks", "ensure"], cwd=repo_dir)
            self.assert_repo_local_hooks(repo_dir)

    def checkpoint_mock_ai(self, repo_dir: Path, files: list[str]) -> None:
        if not files:
            return
        self.run_git_ai(["checkpoint", "mock_ai", *files], cwd=repo_dir)


def seed_basic_repo(runner: VariantRunner, repo_dir: Path, file_count: int = 24) -> list[str]:
    runner.init_repo(repo_dir)
    files: list[str] = []
    for i in range(file_count):
        rel = f"bench/basic/file_{i:03d}.txt"
        write_seed_file(repo_dir / rel, 1000 + i, 70)
        files.append(rel)
    runner.run_git(["add", "-A"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", "seed basic"], cwd=repo_dir)
    return files


def seed_structured_repo(runner: VariantRunner, repo_dir: Path) -> dict[str, list[str]]:
    runner.init_repo(repo_dir)
    groups = {
        "main": [f"bench/main/main_{i:02d}.txt" for i in range(8)],
        "feature": [f"bench/feature/feature_{i:02d}.txt" for i in range(10)],
        "side": [f"bench/side/side_{i:02d}.txt" for i in range(6)],
    }
    seed = 2000
    for files in groups.values():
        for rel in files:
            write_seed_file(repo_dir / rel, seed, 80)
            seed += 1
    runner.run_git(["add", "-A"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", "seed structured"], cwd=repo_dir)
    return groups


def create_ai_commit(
    runner: VariantRunner,
    repo_dir: Path,
    rel_files: list[str],
    marker: str,
    message: str,
) -> None:
    for rel in rel_files:
        append_line(repo_dir / rel, marker)
    runner.checkpoint_mock_ai(repo_dir, rel_files)
    runner.run_git(["add", "-A"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", message], cwd=repo_dir)


def create_plain_commit(
    runner: VariantRunner,
    repo_dir: Path,
    rel_files: list[str],
    marker: str,
    message: str,
) -> None:
    for rel in rel_files:
        append_line(repo_dir / rel, marker)
    runner.run_git(["add", "-A"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", message], cwd=repo_dir)


def setup_human_commit(runner: VariantRunner, repo_dir: Path) -> None:
    seed_basic_repo(runner, repo_dir)


def measure_human_commit(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    files = [f"bench/basic/file_{i:03d}.txt" for i in range(6)]
    for idx, rel in enumerate(files):
        append_line(repo_dir / rel, f"human-change run={run_idx} idx={idx}")
    runner.run_git(["add", "-A"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", f"bench human run {run_idx}"], cwd=repo_dir)


def setup_ai_checkpoint_commit(runner: VariantRunner, repo_dir: Path) -> None:
    seed_basic_repo(runner, repo_dir)


def measure_ai_checkpoint_commit(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    files = [f"bench/basic/file_{i:03d}.txt" for i in range(5)]
    for idx, rel in enumerate(files):
        append_line(repo_dir / rel, f"ai-change run={run_idx} idx={idx}")
    runner.checkpoint_mock_ai(repo_dir, files)
    runner.run_git(["add", "-A"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", f"bench ai commit run {run_idx}"], cwd=repo_dir)


def setup_reset_mixed(runner: VariantRunner, repo_dir: Path) -> None:
    files = seed_basic_repo(runner, repo_dir)
    for i in range(12):
        target = files[i % len(files)]
        create_ai_commit(
            runner,
            repo_dir,
            [target],
            marker=f"history-ai-{i}",
            message=f"history ai commit {i}",
        )


def measure_reset_mixed(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    for i in range(5):
        append_line(repo_dir / f"bench/basic/file_{i:03d}.txt", f"pending-reset-{run_idx}-{i}")
    runner.run_git(["reset", "--mixed", "HEAD~6"], cwd=repo_dir)


def setup_stash_roundtrip(runner: VariantRunner, repo_dir: Path) -> None:
    files = seed_basic_repo(runner, repo_dir)
    create_ai_commit(
        runner,
        repo_dir,
        files[:3],
        marker="seed-ai-stash",
        message="seed ai for stash",
    )


def measure_stash_roundtrip(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    tracked = [f"bench/basic/file_{i:03d}.txt" for i in range(4, 9)]
    for idx, rel in enumerate(tracked):
        append_line(repo_dir / rel, f"stash-tracked-{run_idx}-{idx}")
    runner.checkpoint_mock_ai(repo_dir, tracked[:3])

    untracked = repo_dir / "bench" / f"untracked_{run_idx}.txt"
    write_seed_file(untracked, 7000 + run_idx, 20)

    runner.run_git(["stash", "push", "-u", "-m", f"bench stash {run_idx}"], cwd=repo_dir)
    runner.run_git(["stash", "pop"], cwd=repo_dir)


def setup_cherry_pick_three(runner: VariantRunner, repo_dir: Path) -> None:
    files = seed_basic_repo(runner, repo_dir)
    runner.run_git(["checkout", "-q", "-b", "feature"], cwd=repo_dir)
    for i in range(3):
        create_ai_commit(
            runner,
            repo_dir,
            [files[i]],
            marker=f"feature-cherry-{i}",
            message=f"feature cherry commit {i}",
        )
        runner.run_git(["tag", f"bench-cherry-{i}", "HEAD"], cwd=repo_dir)
    create_plain_commit(
        runner,
        repo_dir,
        [files[10]],
        marker="feature-extra",
        message="feature extra commit",
    )
    runner.run_git(["checkout", "-q", "main"], cwd=repo_dir)
    create_plain_commit(
        runner,
        repo_dir,
        [files[20]],
        marker="main-diverge",
        message="main diverge commit",
    )


def measure_cherry_pick_three(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    commit_ids = [
        runner.run_git(["rev-parse", f"bench-cherry-{i}"], cwd=repo_dir).stdout.strip()
        for i in range(3)
    ]
    if len(commit_ids) != 3:
        raise BenchmarkError("Expected exactly 3 feature commits for cherry-pick scenario")
    runner.run_git(["cherry-pick", *commit_ids], cwd=repo_dir)


def setup_rebase_linear(runner: VariantRunner, repo_dir: Path) -> None:
    groups = seed_structured_repo(runner, repo_dir)
    for i in range(4):
        create_plain_commit(
            runner,
            repo_dir,
            [groups["main"][i % len(groups["main"])]],
            marker=f"main-pre-feature-{i}",
            message=f"main pre feature {i}",
        )

    runner.run_git(["checkout", "-q", "-b", "feature", "main~3"], cwd=repo_dir)
    for i in range(8):
        create_ai_commit(
            runner,
            repo_dir,
            [groups["feature"][i % len(groups["feature"])]],
            marker=f"feature-linear-{i}",
            message=f"feature linear {i}",
        )

    runner.run_git(["checkout", "-q", "main"], cwd=repo_dir)
    for i in range(6):
        create_plain_commit(
            runner,
            repo_dir,
            [groups["main"][(i + 4) % len(groups["main"])]],
            marker=f"main-after-feature-{i}",
            message=f"main after feature {i}",
        )
    runner.run_git(["checkout", "-q", "feature"], cwd=repo_dir)


def measure_rebase_linear(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    runner.run_git(["rebase", "main"], cwd=repo_dir)


def setup_rebase_merges(runner: VariantRunner, repo_dir: Path) -> None:
    groups = seed_structured_repo(runner, repo_dir)
    for i in range(5):
        create_plain_commit(
            runner,
            repo_dir,
            [groups["main"][i % len(groups["main"])]],
            marker=f"main-start-{i}",
            message=f"main start {i}",
        )

    runner.run_git(["checkout", "-q", "-b", "feature", "main~2"], cwd=repo_dir)
    for i in range(6):
        create_ai_commit(
            runner,
            repo_dir,
            [groups["feature"][i % len(groups["feature"])]],
            marker=f"feature-rm-{i}",
            message=f"feature rm {i}",
        )

    runner.run_git(["checkout", "-q", "-b", "side", "feature~3"], cwd=repo_dir)
    for i in range(4):
        create_ai_commit(
            runner,
            repo_dir,
            [groups["side"][i % len(groups["side"])]],
            marker=f"side-rm-{i}",
            message=f"side rm {i}",
        )

    runner.run_git(["checkout", "-q", "feature"], cwd=repo_dir)
    runner.run_git(["merge", "--no-ff", "-q", "-m", "merge side", "side"], cwd=repo_dir)
    for i in range(2):
        create_ai_commit(
            runner,
            repo_dir,
            [groups["feature"][(i + 6) % len(groups["feature"])]],
            marker=f"feature-post-merge-{i}",
            message=f"feature post merge {i}",
        )

    runner.run_git(["checkout", "-q", "main"], cwd=repo_dir)
    for i in range(4):
        create_plain_commit(
            runner,
            repo_dir,
            [groups["main"][(i + 5) % len(groups["main"])]],
            marker=f"main-upstream-{i}",
            message=f"main upstream {i}",
        )
    runner.run_git(["checkout", "-q", "feature"], cwd=repo_dir)


def measure_rebase_merges(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    runner.run_git(["rebase", "--rebase-merges", "main"], cwd=repo_dir)


def setup_squash_merge(runner: VariantRunner, repo_dir: Path) -> None:
    groups = seed_structured_repo(runner, repo_dir)
    runner.run_git(["checkout", "-q", "-b", "feature"], cwd=repo_dir)
    for i in range(10):
        create_ai_commit(
            runner,
            repo_dir,
            [groups["feature"][i % len(groups["feature"])]],
            marker=f"squash-feature-{i}",
            message=f"squash feature {i}",
        )

    runner.run_git(["checkout", "-q", "main"], cwd=repo_dir)
    for i in range(4):
        create_plain_commit(
            runner,
            repo_dir,
            [groups["main"][i % len(groups["main"])]],
            marker=f"squash-main-{i}",
            message=f"squash main {i}",
        )


def measure_squash_merge(runner: VariantRunner, repo_dir: Path, run_idx: int) -> None:
    runner.run_git(["merge", "--squash", "feature"], cwd=repo_dir)
    runner.run_git(["commit", "-q", "-m", f"squash merge run {run_idx}"], cwd=repo_dir)


SCENARIOS = [
    Scenario(
        key="commit_human",
        description="Human-only add/commit on modified tracked files",
        complexity="basic",
        setup=setup_human_commit,
        measure=measure_human_commit,
    ),
    Scenario(
        key="checkpoint_commit_ai",
        description="AI checkpoint + commit flow",
        complexity="basic",
        setup=setup_ai_checkpoint_commit,
        measure=measure_ai_checkpoint_commit,
    ),
    Scenario(
        key="reset_mixed_head6",
        description="Reset mixed with pending worktree edits",
        complexity="basic",
        setup=setup_reset_mixed,
        measure=measure_reset_mixed,
    ),
    Scenario(
        key="stash_roundtrip",
        description="stash push -u + pop on AI-touched and untracked files",
        complexity="basic",
        setup=setup_stash_roundtrip,
        measure=measure_stash_roundtrip,
    ),
    Scenario(
        key="cherry_pick_three",
        description="Cherry-pick three AI commits onto diverged main",
        complexity="basic",
        setup=setup_cherry_pick_three,
        measure=measure_cherry_pick_three,
    ),
    Scenario(
        key="rebase_linear",
        description="Linear feature branch rebase onto updated main",
        complexity="complex",
        setup=setup_rebase_linear,
        measure=measure_rebase_linear,
    ),
    Scenario(
        key="rebase_rebase_merges",
        description="Rebase-merges on branch with merge commit",
        complexity="complex",
        setup=setup_rebase_merges,
        measure=measure_rebase_merges,
    ),
    Scenario(
        key="squash_merge_commit",
        description="merge --squash + commit from feature branch",
        complexity="complex",
        setup=setup_squash_merge,
        measure=measure_squash_merge,
    ),
]


def summarize_runs(results: list[RunResult]) -> dict[str, dict[str, dict[str, float | list[float]]]]:
    grouped: dict[str, dict[str, list[float]]] = {}
    for item in results:
        grouped.setdefault(item.scenario, {}).setdefault(item.variant, []).append(item.duration_ms)

    summary: dict[str, dict[str, dict[str, float | list[float]]]] = {}
    for scenario, by_variant in grouped.items():
        scenario_summary: dict[str, dict[str, float | list[float]]] = {}
        for variant, samples in by_variant.items():
            ordered = sorted(samples)
            median = statistics.median(ordered)
            mean = statistics.mean(ordered)
            stdev = statistics.pstdev(ordered) if len(ordered) > 1 else 0.0
            scenario_summary[variant] = {
                "runs_ms": [round(x, 3) for x in samples],
                "median_ms": round(median, 3),
                "mean_ms": round(mean, 3),
                "min_ms": round(min(ordered), 3),
                "max_ms": round(max(ordered), 3),
                "stdev_ms": round(stdev, 3),
            }
        summary[scenario] = scenario_summary
    return summary


def compute_slowdowns(
    summary: dict[str, dict[str, dict[str, float | list[float]]]],
    baseline_key: str,
) -> dict[str, dict[str, float]]:
    slowdowns: dict[str, dict[str, float]] = {}
    for scenario, by_variant in summary.items():
        if baseline_key not in by_variant:
            continue
        baseline = float(by_variant[baseline_key]["median_ms"])  # type: ignore[index]
        if baseline <= 0:
            continue
        scenario_slowdown: dict[str, float] = {}
        for variant, stats in by_variant.items():
            if variant == baseline_key:
                continue
            med = float(stats["median_ms"])  # type: ignore[index]
            scenario_slowdown[variant] = round(((med - baseline) / baseline) * 100.0, 3)
        slowdowns[scenario] = scenario_slowdown
    return slowdowns


def compute_margin_checks(
    summary: dict[str, dict[str, dict[str, float | list[float]]]],
    *,
    baseline_key: str,
    margin_pct: float,
    variants: list[str],
) -> list[MarginCheckResult]:
    checks: list[MarginCheckResult] = []
    multiplier = 1.0 + (margin_pct / 100.0)
    for scenario, by_variant in summary.items():
        if baseline_key not in by_variant:
            continue
        baseline = float(by_variant[baseline_key]["median_ms"])  # type: ignore[index]
        if baseline <= 0.0:
            continue
        allowed = baseline * multiplier
        for variant in variants:
            if variant not in by_variant:
                continue
            median = float(by_variant[variant]["median_ms"])  # type: ignore[index]
            slowdown = ((median - baseline) / baseline) * 100.0
            checks.append(
                MarginCheckResult(
                    scenario=scenario,
                    variant=variant,
                    baseline_ms=round(baseline, 3),
                    median_ms=round(median, 3),
                    allowed_ms=round(allowed, 3),
                    slowdown_pct=round(slowdown, 3),
                    passed=median <= allowed,
                )
            )
    return checks


def geometric_mean(values: list[float]) -> float:
    if not values:
        return 1.0
    return math.exp(sum(math.log(v) for v in values) / len(values))


def render_report(
    report_path: Path,
    metadata: dict[str, str | int | dict[str, str]],
    scenarios: list[Scenario],
    variants: list[Variant],
    summary: dict[str, dict[str, dict[str, float | list[float]]]],
    slowdowns: dict[str, dict[str, float]],
    margin_checks: list[MarginCheckResult],
) -> None:
    baseline_key = "main_wrapper"
    margin_baseline_key = str(metadata["margin_baseline"])
    margin_baseline_label = margin_baseline_key.replace("_", " ")

    lines: list[str] = []
    lines.append("# git-ai Mode Benchmark Report")
    lines.append("")
    lines.append("## Run Metadata")
    lines.append("")
    lines.append(f"- Timestamp (UTC): `{metadata['timestamp_utc']}`")
    lines.append(f"- Repo: `{metadata['repo_root']}`")
    lines.append(f"- Branch: `{metadata['branch']}`")
    lines.append(f"- Branch SHA: `{metadata['branch_sha']}`")
    lines.append(f"- Main Ref: `{metadata['main_ref']}`")
    lines.append(f"- Main SHA: `{metadata['main_sha']}`")
    lines.append(f"- Real git: `{metadata['real_git']}`")
    lines.append(f"- Iterations (basic): `{metadata['iterations_basic']}`")
    lines.append(f"- Iterations (complex): `{metadata['iterations_complex']}`")
    lines.append("")
    lines.append("## Variants")
    lines.append("")
    for variant in variants:
        lines.append(f"- `{variant.key}`: {variant.label} (`{variant.binary}`)")
    lines.append("")
    lines.append("## Scenario Matrix")
    lines.append("")
    for scenario in scenarios:
        lines.append(f"- `{scenario.key}` ({scenario.complexity}): {scenario.description}")
    lines.append("")
    lines.append("## Exact Timings (ms)")
    lines.append("")
    lines.append(
        "| Scenario | main(wrapper) runs | current(wrapper) runs | current(hooks) runs | current(wrapper+hooks) runs |"
    )
    lines.append("|---|---:|---:|---:|---:|")
    for scenario in scenarios:
        row = [scenario.key]
        for key in ["main_wrapper", "current_wrapper", "current_hooks", "current_both"]:
            runs = summary[scenario.key][key]["runs_ms"]  # type: ignore[index]
            row.append(", ".join(f"{float(v):.3f}" for v in runs))  # type: ignore[arg-type]
        lines.append(f"| {row[0]} | {row[1]} | {row[2]} | {row[3]} | {row[4]} |")
    lines.append("")
    lines.append("## Median Summary (ms) and Slowdown vs main(wrapper)")
    lines.append("")
    lines.append(
        "| Scenario | main(wrapper) | current(wrapper) | current(hooks) | current(wrapper+hooks) | wrapper Δ% | hooks Δ% | both Δ% |"
    )
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for scenario in scenarios:
        data = summary[scenario.key]
        base = float(data["main_wrapper"]["median_ms"])  # type: ignore[index]
        cw = float(data["current_wrapper"]["median_ms"])  # type: ignore[index]
        ch = float(data["current_hooks"]["median_ms"])  # type: ignore[index]
        cb = float(data["current_both"]["median_ms"])  # type: ignore[index]
        s = slowdowns.get(scenario.key, {})
        lines.append(
            f"| {scenario.key} | {base:.3f} | {cw:.3f} | {ch:.3f} | {cb:.3f} | "
            f"{s.get('current_wrapper', 0.0):.3f}% | {s.get('current_hooks', 0.0):.3f}% | {s.get('current_both', 0.0):.3f}% |"
        )

    ratios: dict[str, list[float]] = {
        "current_wrapper": [],
        "current_hooks": [],
        "current_both": [],
    }
    for scenario in scenarios:
        data = summary[scenario.key]
        base = float(data[baseline_key]["median_ms"])  # type: ignore[index]
        for key in ratios:
            med = float(data[key]["median_ms"])  # type: ignore[index]
            ratios[key].append(med / base if base > 0 else 1.0)

    lines.append("")
    lines.append("## Aggregate Comparison")
    lines.append("")
    lines.append("| Variant | Geometric Mean Ratio vs main(wrapper) | Geometric Mean Slowdown |")
    lines.append("|---|---:|---:|")
    for key, ratio_values in ratios.items():
        gm = geometric_mean(ratio_values)
        lines.append(f"| {key} | {gm:.4f}x | {(gm - 1.0) * 100.0:.3f}% |")

    lines.append("")
    lines.append("## Margin Check")
    lines.append("")
    lines.append(
        f"- Required margin: current modes must be <= `{float(metadata['margin_pct']):.1f}%` slower than `{margin_baseline_label}`"
    )
    lines.append(
        "| Scenario | Variant | Baseline (ms) | Variant Median (ms) | Allowed Max (ms) | Slowdown | Status |"
    )
    lines.append("|---|---|---:|---:|---:|---:|---|")
    for check in sorted(margin_checks, key=lambda c: (c.scenario, c.variant)):
        status = "PASS" if check.passed else "FAIL"
        lines.append(
            f"| {check.scenario} | {check.variant} | {check.baseline_ms:.3f} | "
            f"{check.median_ms:.3f} | {check.allowed_ms:.3f} | {check.slowdown_pct:.3f}% | {status} |"
        )
    failed = [check for check in margin_checks if not check.passed]
    lines.append("")
    lines.append(
        f"- Overall: `{len(margin_checks) - len(failed)}/{len(margin_checks)}` checks passing"
    )

    lines.append("")
    lines.append("## Re-run")
    lines.append("")
    lines.append("```bash")
    lines.append(
        "python3 scripts/benchmarks/git/benchmark_modes_vs_main.py --iterations-basic "
        f"{metadata['iterations_basic']} --iterations-complex {metadata['iterations_complex']} "
        f"--margin-pct {float(metadata['margin_pct']):.1f} "
        f"--margin-baseline {metadata['margin_baseline']}"
    )
    lines.append("```")

    report_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def write_raw_csv(path: Path, results: list[RunResult]) -> None:
    with path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(["scenario", "complexity", "variant", "run_index", "duration_ms"])
        for item in results:
            writer.writerow(
                [
                    item.scenario,
                    item.complexity,
                    item.variant,
                    item.run_index,
                    f"{item.duration_ms:.3f}",
                ]
            )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Benchmark main(wrapper) against current wrapper/hooks/wrapper+hooks "
            "across basic and complex git workflows."
        )
    )
    parser.add_argument(
        "--work-root",
        type=Path,
        default=None,
        help="Artifact working directory (defaults to a temp dir).",
    )
    parser.add_argument(
        "--main-ref",
        default="origin/main",
        help="Git ref used for baseline main build (default: origin/main).",
    )
    parser.add_argument(
        "--iterations-basic",
        type=int,
        default=3,
        help="Iterations per basic scenario per variant (default: 3).",
    )
    parser.add_argument(
        "--iterations-complex",
        type=int,
        default=3,
        help="Iterations per complex scenario per variant (default: 3).",
    )
    parser.add_argument(
        "--keep-artifacts",
        action="store_true",
        help="Keep template and run repositories under work-root.",
    )
    parser.add_argument(
        "--current-bin",
        type=Path,
        default=None,
        help="Use an existing current-branch git-ai binary (skip current build).",
    )
    parser.add_argument(
        "--main-bin",
        type=Path,
        default=None,
        help="Use an existing main-branch git-ai binary (skip main build/worktree).",
    )
    parser.add_argument(
        "--margin-pct",
        type=float,
        default=25.0,
        help="Maximum allowed slowdown percentage relative to --margin-baseline.",
    )
    parser.add_argument(
        "--enforce-margin",
        action="store_true",
        help="Exit non-zero when any current_hooks/current_both margin check fails.",
    )
    parser.add_argument(
        "--margin-baseline",
        type=str,
        choices=["current_wrapper", "main_wrapper"],
        default="current_wrapper",
        help="Baseline variant for margin checks.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repo_root = Path(__file__).resolve().parents[3]

    if args.iterations_basic <= 0 or args.iterations_complex <= 0:
        raise BenchmarkError("Iterations must be positive integers.")
    if args.margin_pct < 0:
        raise BenchmarkError("--margin-pct must be non-negative")

    if args.work_root is None:
        work_root = Path(tempfile.mkdtemp(prefix="git-ai-modes-bench-"))
    else:
        work_root = args.work_root.resolve()
        work_root.mkdir(parents=True, exist_ok=True)

    real_git = resolve_real_git_binary(repo_root)

    build_dir = work_root / "build"
    build_dir.mkdir(parents=True, exist_ok=True)
    targets_dir = build_dir / "targets"
    targets_dir.mkdir(parents=True, exist_ok=True)

    main_worktree = build_dir / "main-worktree"
    created_main_worktree = False

    try:
        if args.current_bin is not None:
            current_bin = args.current_bin.resolve()
            if not current_bin.exists():
                raise BenchmarkError(f"Current binary not found: {current_bin}")
        else:
            print("Building current branch binary...")
            current_bin = build_release_binary(repo_root, targets_dir / "current")

        if args.main_bin is not None:
            main_bin = args.main_bin.resolve()
            if not main_bin.exists():
                raise BenchmarkError(f"Main binary not found: {main_bin}")
            main_sha = "unknown (external binary)"
        else:
            print(f"Preparing main worktree at {args.main_ref}...")
            prepare_main_worktree(repo_root, args.main_ref, main_worktree)
            created_main_worktree = True
            print("Building main branch binary...")
            main_bin = build_release_binary(main_worktree, targets_dir / "main")
            main_sha = git_output(main_worktree, ["rev-parse", "HEAD"])

        variants = [
            Variant("main_wrapper", "main(wrapper)", main_bin, "wrapper"),
            Variant("current_wrapper", "current(wrapper)", current_bin, "wrapper"),
            Variant("current_hooks", "current(hooks)", current_bin, "hooks"),
            Variant("current_both", "current(wrapper+hooks)", current_bin, "both"),
        ]

        timestamp = time.strftime("%Y%m%d-%H%M%S", time.localtime())
        artifacts_dir = work_root / "artifacts" / timestamp
        artifacts_dir.mkdir(parents=True, exist_ok=True)

        raw_results: list[RunResult] = []
        templates_root = work_root / "templates"
        runs_root = work_root / "runs"
        templates_root.mkdir(parents=True, exist_ok=True)
        runs_root.mkdir(parents=True, exist_ok=True)

        for scenario in SCENARIOS:
            iterations = (
                args.iterations_basic
                if scenario.complexity == "basic"
                else args.iterations_complex
            )

            for variant in variants:
                scenario_variant_root = templates_root / scenario.key / variant.key
                if scenario_variant_root.exists():
                    shutil.rmtree(scenario_variant_root)
                scenario_variant_root.mkdir(parents=True, exist_ok=True)

                runner = VariantRunner(variant, scenario_variant_root, real_git)
                template_repo = scenario_variant_root / "repo-template"
                print(f"[setup] scenario={scenario.key} variant={variant.key}")
                scenario.setup(runner, template_repo)

                for run_index in range(1, iterations + 1):
                    run_dir = runs_root / scenario.key / variant.key / f"run_{run_index:02d}"
                    if run_dir.exists():
                        shutil.rmtree(run_dir)
                    run_repo = run_dir / "repo"
                    run_repo.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copytree(
                        template_repo,
                        run_repo,
                        ignore=ignore_transient_git_lockfiles,
                    )
                    if variant.mode in ("hooks", "both"):
                        runner.run_git_ai(["git-hooks", "ensure"], cwd=run_repo)
                        runner.assert_repo_local_hooks(run_repo)

                    t0 = time.perf_counter()
                    scenario.measure(runner, run_repo, run_index)
                    duration_ms = (time.perf_counter() - t0) * 1000.0
                    raw_results.append(
                        RunResult(
                            scenario=scenario.key,
                            complexity=scenario.complexity,
                            variant=variant.key,
                            run_index=run_index,
                            duration_ms=duration_ms,
                        )
                    )
                    print(
                        f"[run] scenario={scenario.key} variant={variant.key} "
                        f"run={run_index}/{iterations} duration_ms={duration_ms:.3f}"
                    )

                    if not args.keep_artifacts and run_dir.exists():
                        shutil.rmtree(run_dir)

        summary = summarize_runs(raw_results)
        slowdowns = compute_slowdowns(summary, baseline_key="main_wrapper")
        margin_checks = compute_margin_checks(
            summary,
            baseline_key=args.margin_baseline,
            margin_pct=args.margin_pct,
            variants=["current_hooks", "current_both"],
        )

        metadata: dict[str, str | int | dict[str, str]] = {
            "timestamp_utc": now_iso_utc(),
            "repo_root": str(repo_root),
            "branch": git_output(repo_root, ["rev-parse", "--abbrev-ref", "HEAD"]),
            "branch_sha": git_output(repo_root, ["rev-parse", "HEAD"]),
            "main_ref": args.main_ref,
            "main_sha": main_sha,
            "real_git": str(real_git),
            "iterations_basic": args.iterations_basic,
            "iterations_complex": args.iterations_complex,
            "margin_pct": args.margin_pct,
            "margin_baseline": args.margin_baseline,
            "variants": {v.key: str(v.binary) for v in variants},
        }

        csv_path = artifacts_dir / "raw_results.csv"
        json_path = artifacts_dir / "summary.json"
        md_path = artifacts_dir / "report.md"
        write_raw_csv(csv_path, raw_results)
        json_path.write_text(
            json.dumps(
                {
                    "metadata": metadata,
                    "summary": summary,
                    "slowdowns_pct_vs_main_wrapper": slowdowns,
                    "margin_checks": [dataclasses.asdict(check) for check in margin_checks],
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        render_report(md_path, metadata, SCENARIOS, variants, summary, slowdowns, margin_checks)

        print("")
        print("Benchmark complete")
        print(f"- Report: {md_path}")
        print(f"- JSON:   {json_path}")
        print(f"- CSV:    {csv_path}")
        failed_checks = [check for check in margin_checks if not check.passed]
        print(
            f"- Margin checks: {len(margin_checks) - len(failed_checks)}/{len(margin_checks)} passing"
        )
        if args.enforce_margin and failed_checks:
            print("")
            print("Margin enforcement failed:")
            for check in failed_checks:
                print(
                    f"  - {check.scenario} / {check.variant}: "
                    f"{check.slowdown_pct:.3f}% > {args.margin_pct:.1f}%"
                )
            return 2
        return 0

    finally:
        if created_main_worktree:
            try:
                remove_main_worktree(repo_root, main_worktree)
            except Exception as err:  # noqa: BLE001
                print(f"warning: failed to remove main worktree: {err}", file=sys.stderr)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as err:
        print(f"error: {err}", file=sys.stderr)
        raise SystemExit(1)
