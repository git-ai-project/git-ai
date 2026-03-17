#!/usr/bin/env python3
"""
Benchmark human checkpoint performance when many changed files were not edited by AI.

This reproduces the reported scenario by:
1) creating a synthetic repo
2) optionally seeding a small AI history on a separate subset of files
3) modifying many non-AI files
4) running `git-ai checkpoint` (human) and measuring duration
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path


@dataclass
class RunResult:
    changed_files: int
    run_index: int
    duration_ms: float
    perf_total_ms: int | None
    perf_files_edited: int | None


def run(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        check=True,
        text=True,
        capture_output=capture,
    )


def resolve_git_ai_bin(repo_root: Path, explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.exists():
            raise FileNotFoundError(f"--git-ai-bin does not exist: {path}")
        return path

    candidates = [
        repo_root / "target" / "release" / "git-ai",
        repo_root / "target" / "debug" / "git-ai",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate

    print("Building git-ai binary (cargo build --bin git-ai)...")
    run(["cargo", "build", "--quiet", "--bin", "git-ai"], cwd=repo_root)
    built = repo_root / "target" / "debug" / "git-ai"
    if not built.exists():
        raise FileNotFoundError(
            f"Expected built binary not found at {built}; pass --git-ai-bin explicitly."
        )
    return built


def parse_perf_json(output: str) -> tuple[int | None, int | None]:
    perf_total_ms: int | None = None
    perf_files_edited: int | None = None

    for line in output.splitlines():
        if "[git-ai (perf-json)]" not in line:
            continue
        json_start = line.find("{")
        if json_start < 0:
            continue
        try:
            payload = json.loads(line[json_start:])
        except json.JSONDecodeError:
            continue
        if payload.get("command") == "checkpoint":
            total = payload.get("total_duration_ms")
            files_edited = payload.get("files_edited")
            if isinstance(total, int):
                perf_total_ms = total
            if isinstance(files_edited, int):
                perf_files_edited = files_edited

    return perf_total_ms, perf_files_edited


def median(values: list[float]) -> float:
    if not values:
        return 0.0
    sorted_values = sorted(values)
    n = len(sorted_values)
    mid = n // 2
    if n % 2 == 1:
        return sorted_values[mid]
    return (sorted_values[mid - 1] + sorted_values[mid]) / 2.0


def setup_repo(repo_dir: Path, total_files: int) -> None:
    run(["git", "init", "-q"], cwd=repo_dir)
    run(["git", "config", "user.name", "Benchmark Bot"], cwd=repo_dir)
    run(["git", "config", "user.email", "benchmark@example.com"], cwd=repo_dir)

    base = "line0\nline1\nline2\n"
    for i in range(total_files):
        (repo_dir / f"f{i:05d}.txt").write_text(base, encoding="utf-8")

    run(["git", "add", "."], cwd=repo_dir)
    run(["git", "commit", "-q", "-m", "seed"], cwd=repo_dir)


def seed_ai_history(
    repo_dir: Path,
    git_ai_bin: Path,
    ai_seed_files: int,
    env: dict[str, str],
) -> None:
    if ai_seed_files <= 0:
        return

    ai_files = [f"f{i:05d}.txt" for i in range(ai_seed_files)]
    for file_name in ai_files:
        with (repo_dir / file_name).open("a", encoding="utf-8") as f:
            f.write("ai_seed_line\n")

    cmd = [str(git_ai_bin), "checkpoint", "mock_ai", "--", *ai_files]
    run(cmd, cwd=repo_dir, env=env)
    run(["git", "add", "."], cwd=repo_dir)
    run(["git", "commit", "-q", "-m", "seed ai history"], cwd=repo_dir)


def bench_one_run(
    repo_dir: Path,
    git_ai_bin: Path,
    changed_files: int,
    ai_seed_files: int,
    run_index: int,
    env: dict[str, str],
) -> RunResult:
    start_idx = ai_seed_files
    end_idx = ai_seed_files + changed_files
    human_files = [f"f{i:05d}.txt" for i in range(start_idx, end_idx)]

    for file_name in human_files:
        with (repo_dir / file_name).open("a", encoding="utf-8") as f:
            f.write("human_change_line\n")

    cmd = [str(git_ai_bin), "checkpoint"]
    t0 = time.perf_counter()
    proc = subprocess.run(
        cmd,
        cwd=str(repo_dir),
        env=env,
        check=True,
        text=True,
        capture_output=True,
    )
    duration_ms = (time.perf_counter() - t0) * 1000.0
    combined_output = proc.stdout + "\n" + proc.stderr
    perf_total_ms, perf_files_edited = parse_perf_json(combined_output)

    return RunResult(
        changed_files=changed_files,
        run_index=run_index,
        duration_ms=duration_ms,
        perf_total_ms=perf_total_ms,
        perf_files_edited=perf_files_edited,
    )


def run_scenario(
    *,
    repo_root: Path,
    git_ai_bin: Path,
    total_files: int,
    ai_seed_files: int,
    changed_counts: list[int],
    repeats: int,
    keep_repo: bool,
    inter_commit_move: bool,
) -> None:
    tmp_parent = repo_root / "tmp"
    tmp_parent.mkdir(parents=True, exist_ok=True)
    tmp_root = Path(
        tempfile.mkdtemp(prefix="git-ai-human-non-ai-checkpoint-", dir=str(tmp_parent))
    )

    try:
        base_env = dict(os.environ)
        if inter_commit_move:
            base_env["GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE"] = "true"
        perf_env = {**base_env, "GIT_AI_DEBUG_PERFORMANCE": "2"}

        all_results: list[RunResult] = []
        for changed in changed_counts:
            if ai_seed_files + changed > total_files:
                raise ValueError(
                    f"changed-files ({changed}) + ai-seed-files ({ai_seed_files}) "
                    f"must be <= total-files ({total_files})"
                )

            for i in range(1, repeats + 1):
                repo_dir = tmp_root / f"repo_c{changed}_r{i}"
                repo_dir.mkdir(parents=True, exist_ok=True)
                setup_repo(repo_dir, total_files)
                seed_ai_history(repo_dir, git_ai_bin, ai_seed_files, base_env)

                result = bench_one_run(
                    repo_dir=repo_dir,
                    git_ai_bin=git_ai_bin,
                    changed_files=changed,
                    ai_seed_files=ai_seed_files,
                    run_index=i,
                    env=perf_env,
                )
                all_results.append(result)
                perf_info = (
                    f"perf_total={result.perf_total_ms}ms files_edited={result.perf_files_edited}"
                )
                print(
                    f"changed={changed:5d} run={i:2d} duration={result.duration_ms:8.2f}ms "
                    f"{perf_info}"
                )

                if not keep_repo:
                    shutil.rmtree(repo_dir, ignore_errors=True)

        print("\nSummary (median wall time per changed-file count):")
        print("changed_files,median_ms,median_ms_per_changed_file,median_perf_total_ms")
        for changed in changed_counts:
            bucket = [r for r in all_results if r.changed_files == changed]
            med = median([r.duration_ms for r in bucket])
            med_perf_values = [
                float(r.perf_total_ms) for r in bucket if r.perf_total_ms is not None
            ]
            med_perf = median(med_perf_values) if med_perf_values else 0.0
            ms_per_changed = med / changed if changed else 0.0
            print(f"{changed},{med:.2f},{ms_per_changed:.2f},{med_perf:.2f}")

        print(f"\nBenchmark root directory: {tmp_root}")
    finally:
        if not keep_repo:
            shutil.rmtree(tmp_root, ignore_errors=True)


def parse_counts(raw: str) -> list[int]:
    out: list[int] = []
    for part in raw.split(","):
        part = part.strip()
        if not part:
            continue
        value = int(part)
        if value <= 0:
            raise ValueError(f"Counts must be positive integers, got: {value}")
        out.append(value)
    if not out:
        raise ValueError("At least one changed-file count must be provided.")
    return out


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Benchmark human checkpoint performance for many non-AI changed files."
    )
    parser.add_argument(
        "--git-ai-bin",
        default=None,
        help="Path to git-ai binary. If omitted, tries target/{release,debug}/git-ai.",
    )
    parser.add_argument(
        "--total-files",
        type=int,
        default=5000,
        help="Total files created in the synthetic repo.",
    )
    parser.add_argument(
        "--ai-seed-files",
        type=int,
        default=100,
        help="Files first touched with `checkpoint mock_ai` before human checkpoint tests.",
    )
    parser.add_argument(
        "--changed-counts",
        default="10,100,500,1000,2000",
        help="Comma-separated counts of non-AI files changed before each human checkpoint.",
    )
    parser.add_argument(
        "--repeats",
        type=int,
        default=3,
        help="Number of repetitions per changed-file count.",
    )
    parser.add_argument(
        "--keep-repo",
        action="store_true",
        help="Keep generated benchmark repository under ./tmp for inspection.",
    )
    parser.add_argument(
        "--inter-commit-move",
        action="store_true",
        help="Enable inter-commit move feature flag (adds git blame work per file).",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[3]
    changed_counts = parse_counts(args.changed_counts)
    git_ai_bin = resolve_git_ai_bin(repo_root, args.git_ai_bin)

    print(f"git-ai bin: {git_ai_bin}")
    print(
        "scenario: human checkpoints on non-AI-changed files "
        f"(total_files={args.total_files}, ai_seed_files={args.ai_seed_files}, "
        f"repeats={args.repeats}, inter_commit_move={args.inter_commit_move})"
    )
    run_scenario(
        repo_root=repo_root,
        git_ai_bin=git_ai_bin,
        total_files=args.total_files,
        ai_seed_files=args.ai_seed_files,
        changed_counts=changed_counts,
        repeats=args.repeats,
        keep_repo=args.keep_repo,
        inter_commit_move=args.inter_commit_move,
    )


if __name__ == "__main__":
    main()
