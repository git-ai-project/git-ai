#!/usr/bin/env python3
"""
Benchmark commit latency for workloads with many changed files where only a few are AI-touched.

Scenario per run:
1) Start from a repo with baseline history and a small AI-touched seed set.
2) Make small AI edits to a few files and checkpoint them with mock_ai.
3) Make many human-only edits to additional files.
4) Stage all changes and run `git-ai commit`.
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
    changed_files_total: int
    run_index: int
    wall_ms: float
    total_ms: int | None
    git_ms: int | None
    pre_ms: int | None
    post_ms: int | None


def run(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    capture: bool = True,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        check=False,
        text=True,
        capture_output=capture,
    )
    if proc.returncode != 0:
        stdout = proc.stdout or ""
        stderr = proc.stderr or ""
        raise RuntimeError(
            "Command failed:\n"
            f"cmd: {' '.join(cmd)}\n"
            f"cwd: {cwd}\n"
            f"exit: {proc.returncode}\n"
            f"stdout:\n{stdout}\n"
            f"stderr:\n{stderr}\n"
        )
    return proc


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


def median(values: list[float]) -> float:
    if not values:
        return 0.0
    values = sorted(values)
    n = len(values)
    mid = n // 2
    if n % 2 == 1:
        return values[mid]
    return (values[mid - 1] + values[mid]) / 2.0


def resolve_git_ai_bin(repo_root: Path, explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit).expanduser()
        if not path.exists():
            raise FileNotFoundError(f"--git-ai-bin does not exist: {path}")
        return path

    local_debug = repo_root / "target" / "debug" / "git-ai"
    if local_debug.exists():
        return local_debug

    print("Building local debug binary (cargo build --bin git-ai)...")
    run(["cargo", "build", "--quiet", "--bin", "git-ai"], cwd=repo_root)
    if not local_debug.exists():
        raise FileNotFoundError(f"Expected local debug binary at {local_debug}")
    return local_debug


def parse_commit_perf_json(output: str) -> tuple[int | None, int | None, int | None, int | None]:
    total_ms: int | None = None
    git_ms: int | None = None
    pre_ms: int | None = None
    post_ms: int | None = None

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
        if payload.get("command") != "commit":
            continue

        val = payload.get("total_duration_ms")
        if isinstance(val, int):
            total_ms = val
        val = payload.get("git_duration_ms")
        if isinstance(val, int):
            git_ms = val
        val = payload.get("pre_command_duration_ms")
        if isinstance(val, int):
            pre_ms = val
        val = payload.get("post_command_duration_ms")
        if isinstance(val, int):
            post_ms = val

    return total_ms, git_ms, pre_ms, post_ms


def verify_binary_modes(git_ai_bin: Path, repo_root: Path) -> None:
    # git-ai mode
    git_ai_version = run([str(git_ai_bin), "--version"], cwd=repo_root).stdout.strip()

    # git-wrapper mode (debug-only escape hatch)
    git_version = run(
        [str(git_ai_bin), "--version"],
        cwd=repo_root,
        env={**dict(os.environ), "GIT_AI": "git"},
    ).stdout.strip()

    if "git version " not in git_version:
        raise RuntimeError(
            "Local binary did not enter git-wrapper mode with GIT_AI=git.\n"
            f"binary: {git_ai_bin}\n"
            f"git-ai mode output: {git_ai_version}\n"
            f"wrapper mode output: {git_version}\n"
        )

    print(f"preflight git-ai mode: {git_ai_version}")
    print(f"preflight wrapper mode: {git_version}")


def setup_template_repo(
    template_repo: Path,
    git_ai_bin: Path,
    total_files: int,
    ai_seed_files: int,
    base_env: dict[str, str],
) -> list[str]:
    run(["git", "init", "-q"], cwd=template_repo)
    run(["git", "config", "user.name", "Benchmark Bot"], cwd=template_repo)
    run(["git", "config", "user.email", "benchmark@example.com"], cwd=template_repo)

    base = "line0\nline1\nline2\n"
    for i in range(total_files):
        (template_repo / f"f{i:05d}.txt").write_text(base, encoding="utf-8")

    run(["git", "add", "."], cwd=template_repo)
    run(["git", "commit", "-q", "-m", "seed"], cwd=template_repo)

    ai_seed = [f"f{i:05d}.txt" for i in range(ai_seed_files)]
    if ai_seed:
        for file_name in ai_seed:
            with (template_repo / file_name).open("a", encoding="utf-8") as f:
                f.write("ai_seed_line\n")

        run(
            [str(git_ai_bin), "checkpoint", "mock_ai", "--", *ai_seed],
            cwd=template_repo,
            env=base_env,
        )
        run(["git", "add", "."], cwd=template_repo)
        commit_env = {**base_env, "GIT_AI": "git"}
        run(
            [str(git_ai_bin), "commit", "-m", "seed ai baseline"],
            cwd=template_repo,
            env=commit_env,
        )

    return ai_seed


def modify_files_for_run(
    run_repo: Path,
    *,
    changed_files_total: int,
    ai_files_in_commit: int,
    ai_seed_files: int,
    git_ai_bin: Path,
    base_env: dict[str, str],
) -> None:
    ai_files = [f"f{i:05d}.txt" for i in range(min(ai_files_in_commit, ai_seed_files))]
    for file_name in ai_files:
        with (run_repo / file_name).open("a", encoding="utf-8") as f:
            f.write("ai_current_change\n")

    if ai_files:
        run(
            [str(git_ai_bin), "checkpoint", "mock_ai", "--", *ai_files],
            cwd=run_repo,
            env=base_env,
        )

    human_changes = changed_files_total - len(ai_files)
    if human_changes <= 0:
        return

    start_idx = ai_seed_files
    end_idx = ai_seed_files + human_changes
    for i in range(start_idx, end_idx):
        file_name = f"f{i:05d}.txt"
        with (run_repo / file_name).open("a", encoding="utf-8") as f:
            f.write("human_current_change\n")


def benchmark_commit_once(
    run_repo: Path,
    git_ai_bin: Path,
    changed_files_total: int,
    run_index: int,
    perf_env: dict[str, str],
) -> RunResult:
    run(["git", "add", "."], cwd=run_repo)
    msg = f"bench commit changed={changed_files_total} run={run_index}"

    t0 = time.perf_counter()
    proc = subprocess.run(
        [str(git_ai_bin), "commit", "-m", msg],
        cwd=str(run_repo),
        env={**perf_env, "GIT_AI": "git"},
        text=True,
        check=True,
        capture_output=True,
    )
    wall_ms = (time.perf_counter() - t0) * 1000.0
    output = proc.stdout + "\n" + proc.stderr
    total_ms, git_ms, pre_ms, post_ms = parse_commit_perf_json(output)

    return RunResult(
        changed_files_total=changed_files_total,
        run_index=run_index,
        wall_ms=wall_ms,
        total_ms=total_ms,
        git_ms=git_ms,
        pre_ms=pre_ms,
        post_ms=post_ms,
    )


def run_scenario(
    *,
    repo_root: Path,
    git_ai_bin: Path,
    total_files: int,
    changed_counts: list[int],
    ai_seed_files: int,
    ai_files_in_commit: int,
    repeats: int,
    keep_repos: bool,
) -> None:
    if ai_files_in_commit > ai_seed_files:
        raise ValueError(
            f"--ai-files-in-commit ({ai_files_in_commit}) must be <= --ai-seed-files ({ai_seed_files})"
        )
    if max(changed_counts) > total_files:
        raise ValueError(
            f"largest changed-count ({max(changed_counts)}) must be <= total-files ({total_files})"
        )
    if max(changed_counts) > (total_files - ai_seed_files + ai_files_in_commit):
        raise ValueError(
            "changed-count exceeds available human-file pool; increase --total-files or lower counts"
        )

    tmp_parent = repo_root / "tmp"
    tmp_parent.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="git-ai-commit-mostly-human-", dir=str(tmp_parent)))
    template_repo = root / "template"
    template_repo.mkdir(parents=True, exist_ok=True)

    base_env = dict(os.environ)
    perf_env = {**base_env, "GIT_AI_DEBUG_PERFORMANCE": "2"}

    try:
        setup_template_repo(template_repo, git_ai_bin, total_files, ai_seed_files, base_env)

        results: list[RunResult] = []
        for changed in changed_counts:
            for i in range(1, repeats + 1):
                run_repo = root / f"run_c{changed}_r{i}"
                run(
                    ["git", "clone", "-q", str(template_repo), str(run_repo)],
                    cwd=root,
                )

                modify_files_for_run(
                    run_repo,
                    changed_files_total=changed,
                    ai_files_in_commit=ai_files_in_commit,
                    ai_seed_files=ai_seed_files,
                    git_ai_bin=git_ai_bin,
                    base_env=base_env,
                )

                result = benchmark_commit_once(run_repo, git_ai_bin, changed, i, perf_env)
                results.append(result)

                print(
                    f"changed={changed:5d} run={i:2d} wall={result.wall_ms:8.2f}ms "
                    f"total={result.total_ms}ms git={result.git_ms}ms "
                    f"pre={result.pre_ms}ms post={result.post_ms}ms"
                )

                if not keep_repos:
                    shutil.rmtree(run_repo, ignore_errors=True)

        print("\nSummary (median):")
        print("changed,wall_ms,total_ms,git_ms,pre_ms,post_ms,overhead_ms")
        for changed in changed_counts:
            bucket = [r for r in results if r.changed_files_total == changed]
            wall = median([r.wall_ms for r in bucket])
            total = median([float(r.total_ms or 0) for r in bucket])
            git = median([float(r.git_ms or 0) for r in bucket])
            pre = median([float(r.pre_ms or 0) for r in bucket])
            post = median([float(r.post_ms or 0) for r in bucket])
            overhead = pre + post
            print(f"{changed},{wall:.2f},{total:.2f},{git:.2f},{pre:.2f},{post:.2f},{overhead:.2f}")

        print(f"\nBenchmark root directory: {root}")
    finally:
        if not keep_repos:
            shutil.rmtree(root, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(
        description=(
            "Benchmark `git-ai commit` with thousands of changed files, mostly human, "
            "plus a few AI-touched files."
        )
    )
    parser.add_argument(
        "--git-ai-bin",
        default=None,
        help=(
            "Path to git-ai binary. Defaults to this repo's local debug binary "
            "(target/debug/git-ai)."
        ),
    )
    parser.add_argument("--total-files", type=int, default=7000)
    parser.add_argument("--changed-counts", default="1000,3000,5000")
    parser.add_argument(
        "--ai-seed-files",
        type=int,
        default=20,
        help="Files with AI history in baseline repo.",
    )
    parser.add_argument(
        "--ai-files-in-commit",
        type=int,
        default=5,
        help="AI-touched files in each measured commit.",
    )
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--keep-repos", action="store_true")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[3]
    git_ai_bin = resolve_git_ai_bin(repo_root, args.git_ai_bin)
    changed_counts = parse_counts(args.changed_counts)

    print(f"git-ai binary: {git_ai_bin}")
    verify_binary_modes(git_ai_bin, repo_root)
    print(
        "scenario: commit with mostly human changes "
        f"(total_files={args.total_files}, changed_counts={changed_counts}, "
        f"ai_seed_files={args.ai_seed_files}, ai_files_in_commit={args.ai_files_in_commit}, "
        f"repeats={args.repeats})"
    )

    run_scenario(
        repo_root=repo_root,
        git_ai_bin=git_ai_bin,
        total_files=args.total_files,
        changed_counts=changed_counts,
        ai_seed_files=args.ai_seed_files,
        ai_files_in_commit=args.ai_files_in_commit,
        repeats=args.repeats,
        keep_repos=args.keep_repos,
    )


if __name__ == "__main__":
    main()
