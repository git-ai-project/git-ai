#!/usr/bin/env python3
"""Run brutal checkpoint-only stress benchmarks with absolute p99 thresholds."""

from __future__ import annotations

import argparse
import csv
import dataclasses
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


class BenchmarkError(RuntimeError):
    pass


@dataclasses.dataclass(frozen=True)
class ScenarioConfig:
    key: str
    description: str
    files: int
    initial_lines: int
    series_per_file: int
    threshold_p99_ms: float
    commit_every: int
    history_commits: int
    burst_probability: float


@dataclasses.dataclass(frozen=True)
class MutationProfile:
    min_lines: int
    max_lines: int
    add_min: int
    add_max: int
    delete_min: int
    delete_max: int
    rewrite_min: int
    rewrite_max: int
    full_replace_probability: float
    multi_op_min: int
    multi_op_max: int


@dataclasses.dataclass
class CheckpointSample:
    scenario: str
    sample_index: int
    series_index: int
    primary_file: str
    files_touched: int
    line_count_before: int
    line_count_after: int
    duration_ms: float
    operations: str


WORDS_A = [
    "vector",
    "cache",
    "index",
    "cursor",
    "worker",
    "session",
    "profile",
    "payload",
    "record",
    "window",
    "bundle",
    "token",
    "format",
    "result",
    "signal",
    "metric",
    "stream",
    "writer",
    "reader",
    "buffer",
]

WORDS_B = [
    "alpha",
    "beta",
    "delta",
    "omega",
    "forest",
    "bridge",
    "sierra",
    "ember",
    "violet",
    "silver",
    "topaz",
    "granite",
    "north",
    "south",
    "east",
    "west",
    "rapid",
    "steady",
    "bright",
    "silent",
]

PUNCT = [";", "::", "=>", "->", "|", "&&", "||", "==", "!=", "<=", ">="]


def now_iso_utc() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def run_cmd(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout_s: int = 3600,
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
    if "git-ai" in fallback_path.name.lower() or str(repo_root / "target") in str(fallback_path):
        raise BenchmarkError(
            "Resolved `git` points to a git-ai wrapper, not the real git binary."
        )
    return fallback_path


def build_release_binary(repo_dir: Path, target_dir: Path) -> Path:
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    run_cmd(
        ["cargo", "build", "--release", "--bin", "git-ai"],
        cwd=repo_dir,
        env=env,
        timeout_s=3600,
    )
    binary = target_dir / "release" / ("git-ai.exe" if os.name == "nt" else "git-ai")
    if not binary.exists():
        raise BenchmarkError(f"Expected binary not found: {binary}")
    return binary


def git_output(repo_dir: Path, args: list[str], env: dict[str, str]) -> str:
    proc = run_cmd(["git", *args], cwd=repo_dir, env=env, timeout_s=120)
    return (proc.stdout or "").strip()


def percentile_nearest_rank(samples: list[float], p: float) -> float:
    if not samples:
        return 0.0
    if p <= 0:
        return min(samples)
    if p >= 100:
        return max(samples)
    ordered = sorted(samples)
    rank = int((p / 100.0) * len(ordered))
    if rank <= 0:
        return ordered[0]
    idx = min(rank - 1, len(ordered) - 1)
    return ordered[idx]


def summarize_samples(samples: list[float]) -> dict[str, float | int]:
    if not samples:
        return {
            "count": 0,
            "min_ms": 0.0,
            "max_ms": 0.0,
            "mean_ms": 0.0,
            "median_ms": 0.0,
            "p90_ms": 0.0,
            "p95_ms": 0.0,
            "p99_ms": 0.0,
        }

    return {
        "count": len(samples),
        "min_ms": round(min(samples), 3),
        "max_ms": round(max(samples), 3),
        "mean_ms": round(statistics.mean(samples), 3),
        "median_ms": round(statistics.median(samples), 3),
        "p90_ms": round(percentile_nearest_rank(samples, 90), 3),
        "p95_ms": round(percentile_nearest_rank(samples, 95), 3),
        "p99_ms": round(percentile_nearest_rank(samples, 99), 3),
    }


class ContentFactory:
    def __init__(self, rng: random.Random) -> None:
        self.rng = rng
        self.counter = 0

    def _word(self) -> str:
        return f"{self.rng.choice(WORDS_A)}_{self.rng.choice(WORDS_B)}"

    def next_line(self, file_idx: int) -> str:
        self.counter += 1
        indent = " " * self.rng.choice([0, 0, 2, 4, 8])
        if self.rng.random() < 0.06:
            indent = "\t" + indent
        suffix_ws = " " * self.rng.choice([0, 0, 0, 1, 2, 3])

        kind = self.rng.random()
        n1 = self.rng.randint(1, 9999)
        n2 = self.rng.randint(1, 9999)
        tok = self._word()
        tok2 = self._word()
        punct = self.rng.choice(PUNCT)

        if kind < 0.18:
            line = (
                f"{indent}fn {tok}(left_{n1}: i64, right_{n2}: i64) -> i64 "
                f"{{ left_{n1} {punct} right_{n2}; }}"
            )
        elif kind < 0.36:
            line = (
                f"{indent}const {tok.upper()}_{file_idx:03d}_{self.counter:07d} = "
                f"\"{tok2}:{n1}:{n2}\";"
            )
        elif kind < 0.53:
            line = (
                f"{indent}{{\"file\":{file_idx},\"seq\":{self.counter},"
                f"\"name\":\"{tok}\",\"payload\":\"{tok2}\",\"ok\":true}}"
            )
        elif kind < 0.7:
            line = (
                f"{indent}[{tok}] level=INFO req={n1:04d} user={tok2} "
                f"latency_ms={self.rng.randint(1, 900)} status={self.rng.choice([200,201,204,400,404,500])}"
            )
        elif kind < 0.86:
            line = (
                f"{indent}SELECT {tok}, {tok2} FROM table_{file_idx % 17} "
                f"WHERE key_{n1 % 19} = '{tok2}' AND bucket_{n2 % 13} > {self.rng.randint(0, 1000)};"
            )
        else:
            line = (
                f"{indent}# note {tok} :: scenario_{file_idx % 9} "
                f"sequence={self.counter} text=\"{tok2} keeps evolving under heavy edits\""
            )

        if self.rng.random() < 0.08:
            line = line.replace(" ", "  ")
        if self.rng.random() < 0.03:
            line = line.replace("=", " = ")

        return f"{line}{suffix_ws}"

    def block(self, file_idx: int, count: int) -> list[str]:
        return [self.next_line(file_idx) for _ in range(max(0, count))]


def write_lines(path: Path, lines: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = "\n".join(lines)
    if text:
        text += "\n"
    path.write_text(text, encoding="utf-8")


def enforce_bounds(
    rng: random.Random,
    lines: list[str],
    profile: MutationProfile,
    factory: ContentFactory,
    file_idx: int,
) -> None:
    if len(lines) > profile.max_lines:
        overflow = len(lines) - profile.max_lines
        while overflow > 0 and lines:
            chunk = min(overflow, max(1, overflow // 3))
            start = 0
            if len(lines) > chunk:
                start = rng.randint(0, len(lines) - chunk)
            del lines[start : start + chunk]
            overflow = len(lines) - profile.max_lines

    if len(lines) < profile.min_lines:
        deficit = profile.min_lines - len(lines)
        lines.extend(factory.block(file_idx, deficit))


def mutate_lines(
    *,
    rng: random.Random,
    lines: list[str],
    file_idx: int,
    profile: MutationProfile,
    factory: ContentFactory,
) -> list[str]:
    operations: list[str] = []

    if not lines:
        lines.extend(factory.block(file_idx, profile.min_lines))

    op_count = rng.randint(profile.multi_op_min, profile.multi_op_max)
    if rng.random() < profile.full_replace_probability:
        replacement_len = rng.randint(profile.min_lines, profile.max_lines)
        lines[:] = factory.block(file_idx, replacement_len)
        operations.append(f"full_replace:{replacement_len}")
        return operations

    for _ in range(op_count):
        available_delete = max(0, len(lines) - profile.min_lines)
        room_to_add = max(0, profile.max_lines - len(lines))

        op = rng.choices(
            population=[
                "add",
                "delete",
                "rewrite",
                "move",
                "whitespace",
                "duplicate",
            ],
            weights=[28, 18, 24, 10, 10, 10],
            k=1,
        )[0]

        if op == "add" and room_to_add > 0:
            max_size = min(profile.add_max, room_to_add)
            if max_size <= 0:
                continue
            if max_size < profile.add_min:
                size = max_size
            else:
                size = rng.randint(profile.add_min, max_size)
            at = rng.randint(0, len(lines))
            lines[at:at] = factory.block(file_idx, size)
            operations.append(f"add:{size}@{at}")
        elif op == "delete" and available_delete >= profile.delete_min:
            max_size = min(profile.delete_max, available_delete)
            size = rng.randint(profile.delete_min, max_size)
            start = rng.randint(0, len(lines) - size)
            del lines[start : start + size]
            operations.append(f"delete:{size}@{start}")
        elif op == "rewrite" and lines:
            max_span = min(profile.rewrite_max, len(lines))
            if max_span <= 0:
                continue
            if max_span < profile.rewrite_min:
                span = max_span
            else:
                span = rng.randint(profile.rewrite_min, max_span)
            start = rng.randint(0, len(lines) - span)
            replacement = rng.randint(
                max(1, int(span * 0.5)),
                max(1, int(span * 1.5)),
            )
            replacement = min(replacement, profile.max_lines)
            lines[start : start + span] = factory.block(file_idx, replacement)
            operations.append(f"rewrite:{span}->{replacement}@{start}")
        elif op == "move" and len(lines) > 6:
            span = rng.randint(1, min(250, max(1, len(lines) // 20)))
            start = rng.randint(0, len(lines) - span)
            block = lines[start : start + span]
            del lines[start : start + span]
            dest = rng.randint(0, len(lines))
            lines[dest:dest] = block
            operations.append(f"move:{span}:{start}->{dest}")
        elif op == "duplicate" and len(lines) > 4 and room_to_add > 0:
            span = rng.randint(1, min(200, len(lines) // 10 + 1, room_to_add))
            start = rng.randint(0, len(lines) - span)
            dest = rng.randint(0, len(lines))
            lines[dest:dest] = lines[start : start + span]
            operations.append(f"duplicate:{span}:{start}->{dest}")
        elif op == "whitespace" and lines:
            touches = rng.randint(1, min(120, len(lines)))
            for _ in range(touches):
                idx = rng.randint(0, len(lines) - 1)
                text = lines[idx]
                if rng.random() < 0.5:
                    text = text.rstrip()
                    text += " " * rng.choice([0, 1, 2, 3, 4])
                else:
                    if text.startswith("\t"):
                        text = text[1:]
                    elif text.startswith("    "):
                        text = text[4:]
                    else:
                        text = "    " + text
                lines[idx] = text
            operations.append(f"whitespace:{touches}")

    enforce_bounds(rng, lines, profile, factory, file_idx)
    return operations


class RepoRunner:
    def __init__(
        self,
        *,
        repo_dir: Path,
        git_ai_bin: Path,
        real_git: Path,
        env: dict[str, str],
    ) -> None:
        self.repo_dir = repo_dir
        self.git_ai_bin = git_ai_bin
        self.real_git = real_git
        self.env = env

    def init_repo(self) -> None:
        self.repo_dir.mkdir(parents=True, exist_ok=True)
        run_cmd([str(self.real_git), "init", "-q", "-b", "main"], cwd=self.repo_dir, env=self.env)
        self.git(["config", "user.name", "Checkpoint Benchmark Bot"])
        self.git(["config", "user.email", "checkpoint-benchmark@git-ai.local"])

    def git(self, args: list[str], timeout_s: int = 900) -> subprocess.CompletedProcess[str]:
        return run_cmd([str(self.real_git), *args], cwd=self.repo_dir, env=self.env, timeout_s=timeout_s)

    def checkpoint_mock_ai(self, rel_files: list[str], timeout_s: int = 900) -> float:
        if not rel_files:
            return 0.0
        t0 = time.perf_counter()
        run_cmd(
            [str(self.git_ai_bin), "checkpoint", "mock_ai", *rel_files],
            cwd=self.repo_dir,
            env=self.env,
            timeout_s=timeout_s,
        )
        return (time.perf_counter() - t0) * 1000.0

    def commit_if_dirty(self, message: str) -> bool:
        status = self.git(["status", "--porcelain"]).stdout.strip()
        if not status:
            return False
        self.git(["add", "-A"])
        self.git(["commit", "-q", "-m", message], timeout_s=1200)
        return True


def scenario_files(scenario: str, file_count: int) -> list[str]:
    return [f"bench/{scenario}/file_{idx:03d}.txt" for idx in range(file_count)]


def seed_repo_content(
    *,
    runner: RepoRunner,
    scenario: ScenarioConfig,
    rng: random.Random,
    factory: ContentFactory,
) -> dict[str, list[str]]:
    file_state: dict[str, list[str]] = {}
    for idx, rel in enumerate(scenario_files(scenario.key, scenario.files)):
        lines = factory.block(idx, scenario.initial_lines)
        write_lines(runner.repo_dir / rel, lines)
        file_state[rel] = lines
    runner.git(["add", "-A"])
    runner.git(["commit", "-q", "-m", f"seed {scenario.key}"])

    # Optional history build-up for long-history scenario.
    for commit_idx in range(1, scenario.history_commits + 1):
        touched = rng.sample(list(file_state.keys()), k=min(4, len(file_state)))
        history_profile = MutationProfile(
            min_lines=max(50, scenario.initial_lines // 2),
            max_lines=max(120, int(scenario.initial_lines * 1.4)),
            add_min=5,
            add_max=max(20, scenario.initial_lines // 30),
            delete_min=3,
            delete_max=max(20, scenario.initial_lines // 25),
            rewrite_min=5,
            rewrite_max=max(30, scenario.initial_lines // 20),
            full_replace_probability=0.01,
            multi_op_min=1,
            multi_op_max=2,
        )
        for rel in touched:
            file_idx = int(rel.rsplit("_", 1)[-1].split(".")[0])
            mutate_lines(
                rng=rng,
                lines=file_state[rel],
                file_idx=file_idx,
                profile=history_profile,
                factory=factory,
            )
            write_lines(runner.repo_dir / rel, file_state[rel])

        runner.checkpoint_mock_ai(touched, timeout_s=2400)
        runner.commit_if_dirty(f"history-{scenario.key}-{commit_idx:04d}")

        if commit_idx % 25 == 0:
            print(
                f"[history] scenario={scenario.key} commits={commit_idx}/{scenario.history_commits}",
                flush=True,
            )

    return file_state


def run_scenario(
    *,
    runner: RepoRunner,
    scenario: ScenarioConfig,
    rng: random.Random,
    factory: ContentFactory,
    profile: MutationProfile,
) -> list[CheckpointSample]:
    file_state = seed_repo_content(runner=runner, scenario=scenario, rng=rng, factory=factory)
    rel_files = list(file_state.keys())

    samples: list[CheckpointSample] = []
    sample_index = 0
    commits_made = 0

    for series_idx in range(1, scenario.series_per_file + 1):
        ordered = list(rel_files)
        rng.shuffle(ordered)
        for rel in ordered:
            primary_idx = int(rel.rsplit("_", 1)[-1].split(".")[0])
            touched = [rel]

            if scenario.burst_probability > 0 and rng.random() < scenario.burst_probability:
                extra_count = rng.randint(1, min(6, max(1, len(rel_files) - 1)))
                extras = rng.sample([p for p in rel_files if p != rel], k=extra_count)
                touched.extend(extras)

            before = len(file_state[rel])
            all_ops: list[str] = []
            for touched_rel in touched:
                idx = int(touched_rel.rsplit("_", 1)[-1].split(".")[0])
                ops = mutate_lines(
                    rng=rng,
                    lines=file_state[touched_rel],
                    file_idx=idx,
                    profile=profile,
                    factory=factory,
                )
                all_ops.extend(f"{Path(touched_rel).name}:{op}" for op in ops)
                write_lines(runner.repo_dir / touched_rel, file_state[touched_rel])

            duration_ms = runner.checkpoint_mock_ai(touched, timeout_s=3600)
            after = len(file_state[rel])
            sample_index += 1

            samples.append(
                CheckpointSample(
                    scenario=scenario.key,
                    sample_index=sample_index,
                    series_index=series_idx,
                    primary_file=rel,
                    files_touched=len(touched),
                    line_count_before=before,
                    line_count_after=after,
                    duration_ms=duration_ms,
                    operations=" | ".join(all_ops[:10]),
                )
            )

            if scenario.commit_every > 0 and (sample_index % scenario.commit_every) == 0:
                if runner.commit_if_dirty(
                    f"checkpoint-{scenario.key}-series-{series_idx:04d}-sample-{sample_index:06d}"
                ):
                    commits_made += 1

            if sample_index % 100 == 0:
                print(
                    f"[progress] scenario={scenario.key} sample={sample_index}/"
                    f"{scenario.files * scenario.series_per_file} commits={commits_made}",
                    flush=True,
                )

    if runner.commit_if_dirty(f"checkpoint-{scenario.key}-finalize"):
        commits_made += 1

    print(
        f"[done] scenario={scenario.key} samples={len(samples)} commits={commits_made}",
        flush=True,
    )
    return samples


def write_samples_csv(path: Path, samples: list[CheckpointSample]) -> None:
    with path.open("w", encoding="utf-8", newline="") as fh:
        writer = csv.writer(fh)
        writer.writerow(
            [
                "scenario",
                "sample_index",
                "series_index",
                "primary_file",
                "files_touched",
                "line_count_before",
                "line_count_after",
                "duration_ms",
                "operations",
            ]
        )
        for sample in samples:
            writer.writerow(
                [
                    sample.scenario,
                    sample.sample_index,
                    sample.series_index,
                    sample.primary_file,
                    sample.files_touched,
                    sample.line_count_before,
                    sample.line_count_after,
                    f"{sample.duration_ms:.3f}",
                    sample.operations,
                ]
            )


def write_report(
    path: Path,
    *,
    metadata: dict[str, Any],
    scenario_results: dict[str, dict[str, Any]],
) -> None:
    lines: list[str] = []
    lines.append("# git-ai Checkpoint Stress Benchmark")
    lines.append("")
    lines.append("## Run Metadata")
    lines.append("")
    lines.append(f"- Timestamp (UTC): `{metadata['timestamp_utc']}`")
    lines.append(f"- Repo root: `{metadata['repo_root']}`")
    lines.append(f"- Branch: `{metadata['branch']}`")
    lines.append(f"- Branch SHA: `{metadata['branch_sha']}`")
    lines.append(f"- git-ai binary: `{metadata['git_ai_bin']}`")
    lines.append(f"- Real git: `{metadata['real_git']}`")
    lines.append(f"- Seed: `{metadata['seed']}`")
    lines.append("")

    lines.append("## Scenario Results")
    lines.append("")
    lines.append("| Scenario | Samples | p50 (ms) | p90 (ms) | p95 (ms) | p99 (ms) | max (ms) | Threshold p99 (ms) | Status |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|---|")

    for key, result in scenario_results.items():
        stats = result["stats"]
        status = "PASS" if result["passed"] else "FAIL"
        lines.append(
            f"| {key} | {stats['count']} | {stats['median_ms']:.3f} | {stats['p90_ms']:.3f} | "
            f"{stats['p95_ms']:.3f} | {stats['p99_ms']:.3f} | {stats['max_ms']:.3f} | "
            f"{result['threshold_p99_ms']:.3f} | {status} |"
        )

    lines.append("")
    lines.append(f"## Overall: {'PASS' if metadata['overall_passed'] else 'FAIL'}")
    lines.append("")
    lines.append("## Re-run")
    lines.append("")
    lines.append("```bash")
    lines.append("python3 scripts/benchmarks/git/benchmark_checkpoint_stress.py --enforce-thresholds")
    lines.append("```")

    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run heavy checkpoint-only stress benchmarks with absolute p99 gates."
    )
    parser.add_argument("--work-root", type=Path, default=None)
    parser.add_argument("--seed", type=int, default=20260304)

    parser.add_argument("--basic-files", type=int, default=100)
    parser.add_argument("--basic-lines", type=int, default=800)
    parser.add_argument("--basic-series-per-file", type=int, default=20)
    parser.add_argument("--basic-threshold-ms", type=float, default=100.0)
    parser.add_argument("--basic-commit-every", type=int, default=40)

    parser.add_argument("--churn-files", type=int, default=100)
    parser.add_argument("--churn-lines", type=int, default=10000)
    parser.add_argument("--churn-series-per-file", type=int, default=100)
    parser.add_argument("--churn-threshold-ms", type=float, default=250.0)
    parser.add_argument("--churn-commit-every", type=int, default=25)
    parser.add_argument("--churn-history-commits", type=int, default=300)

    parser.add_argument("--git-ai-bin", type=Path, default=None)
    parser.add_argument("--enforce-thresholds", action="store_true")
    parser.add_argument("--keep-artifacts", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repo_root = Path(__file__).resolve().parents[3]

    if args.basic_lines >= 1000:
        raise BenchmarkError("--basic-lines must be <1000 by requirement")
    if args.basic_files <= 0 or args.churn_files <= 0:
        raise BenchmarkError("File counts must be positive")
    if args.basic_series_per_file <= 0 or args.churn_series_per_file <= 0:
        raise BenchmarkError("Series-per-file must be positive")

    if args.work_root is None:
        work_root = Path(tempfile.mkdtemp(prefix="git-ai-checkpoint-stress-"))
    else:
        work_root = args.work_root.resolve()
        work_root.mkdir(parents=True, exist_ok=True)

    build_root = work_root / "build"
    build_root.mkdir(parents=True, exist_ok=True)

    if args.git_ai_bin is not None:
        git_ai_bin = args.git_ai_bin.resolve()
        if not git_ai_bin.exists():
            raise BenchmarkError(f"git-ai binary not found: {git_ai_bin}")
    else:
        print("Building current git-ai release binary...", flush=True)
        git_ai_bin = build_release_binary(repo_root, build_root / "target")

    real_git = resolve_real_git_binary(repo_root)

    run_env = dict(os.environ)
    run_env["GIT_TERMINAL_PROMPT"] = "0"
    run_env["GIT_AI_DEBUG"] = "0"
    run_env["GIT_AI_DEBUG_PERFORMANCE"] = "0"

    scenarios = [
        ScenarioConfig(
            key="basic_checkpoint",
            description=(
                "Basic checkpoint stress with ~100 sub-1k files and varied bursty edits"
            ),
            files=args.basic_files,
            initial_lines=args.basic_lines,
            series_per_file=args.basic_series_per_file,
            threshold_p99_ms=args.basic_threshold_ms,
            commit_every=args.basic_commit_every,
            history_commits=0,
            burst_probability=0.18,
        ),
        ScenarioConfig(
            key="churn_long_history_checkpoint",
            description=(
                "Long-history churn benchmark with 100 files x 10k lines x 100 checkpoint series"
            ),
            files=args.churn_files,
            initial_lines=args.churn_lines,
            series_per_file=args.churn_series_per_file,
            threshold_p99_ms=args.churn_threshold_ms,
            commit_every=args.churn_commit_every,
            history_commits=args.churn_history_commits,
            burst_probability=0.0,
        ),
    ]

    basic_profile = MutationProfile(
        min_lines=max(200, args.basic_lines // 2),
        max_lines=980,
        add_min=3,
        add_max=80,
        delete_min=2,
        delete_max=70,
        rewrite_min=3,
        rewrite_max=120,
        full_replace_probability=0.03,
        multi_op_min=1,
        multi_op_max=4,
    )

    churn_profile = MutationProfile(
        min_lines=max(7000, int(args.churn_lines * 0.7)),
        max_lines=max(12000, int(args.churn_lines * 1.2)),
        add_min=20,
        add_max=800,
        delete_min=10,
        delete_max=700,
        rewrite_min=25,
        rewrite_max=1200,
        full_replace_probability=0.05,
        multi_op_min=2,
        multi_op_max=6,
    )

    scenario_profiles = {
        "basic_checkpoint": basic_profile,
        "churn_long_history_checkpoint": churn_profile,
    }

    timestamp = time.strftime("%Y%m%d-%H%M%S", time.localtime())
    artifacts_dir = work_root / "artifacts" / timestamp
    artifacts_dir.mkdir(parents=True, exist_ok=True)

    rng = random.Random(args.seed)
    factory = ContentFactory(rng)

    all_samples: list[CheckpointSample] = []
    scenario_results: dict[str, dict[str, Any]] = {}

    for scenario in scenarios:
        scenario_root = work_root / "runs" / scenario.key
        if scenario_root.exists():
            shutil.rmtree(scenario_root)
        scenario_root.mkdir(parents=True, exist_ok=True)

        runner = RepoRunner(
            repo_dir=scenario_root / "repo",
            git_ai_bin=git_ai_bin,
            real_git=real_git,
            env=run_env,
        )
        runner.init_repo()

        print(
            f"[scenario-start] {scenario.key} files={scenario.files} lines={scenario.initial_lines} "
            f"series_per_file={scenario.series_per_file} history_commits={scenario.history_commits}",
            flush=True,
        )

        samples = run_scenario(
            runner=runner,
            scenario=scenario,
            rng=rng,
            factory=factory,
            profile=scenario_profiles[scenario.key],
        )

        durations = [sample.duration_ms for sample in samples]
        stats = summarize_samples(durations)
        p99 = float(stats["p99_ms"])
        passed = p99 < scenario.threshold_p99_ms

        scenario_results[scenario.key] = {
            "description": scenario.description,
            "threshold_p99_ms": scenario.threshold_p99_ms,
            "stats": stats,
            "passed": passed,
        }

        all_samples.extend(samples)

    overall_passed = all(item["passed"] for item in scenario_results.values())

    metadata: dict[str, Any] = {
        "timestamp_utc": now_iso_utc(),
        "repo_root": str(repo_root),
        "branch": git_output(repo_root, ["rev-parse", "--abbrev-ref", "HEAD"], run_env),
        "branch_sha": git_output(repo_root, ["rev-parse", "HEAD"], run_env),
        "git_ai_bin": str(git_ai_bin),
        "real_git": str(real_git),
        "seed": args.seed,
        "work_root": str(work_root),
        "overall_passed": overall_passed,
        "parameters": {
            "basic": {
                "files": args.basic_files,
                "lines": args.basic_lines,
                "series_per_file": args.basic_series_per_file,
                "threshold_ms": args.basic_threshold_ms,
            },
            "churn": {
                "files": args.churn_files,
                "lines": args.churn_lines,
                "series_per_file": args.churn_series_per_file,
                "threshold_ms": args.churn_threshold_ms,
                "history_commits": args.churn_history_commits,
            },
        },
    }

    csv_path = artifacts_dir / "raw_checkpoint_samples.csv"
    json_path = artifacts_dir / "summary.json"
    report_path = artifacts_dir / "report.md"

    write_samples_csv(csv_path, all_samples)
    json_path.write_text(
        json.dumps(
            {
                "metadata": metadata,
                "scenarios": scenario_results,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    write_report(report_path, metadata=metadata, scenario_results=scenario_results)

    print("")
    print("Checkpoint stress benchmark complete")
    print(f"- Report: {report_path}")
    print(f"- JSON:   {json_path}")
    print(f"- CSV:    {csv_path}")

    for scenario_key, result in scenario_results.items():
        stats = result["stats"]
        print(
            f"- {scenario_key}: p99={stats['p99_ms']:.3f}ms "
            f"threshold<{result['threshold_p99_ms']:.3f}ms status={'PASS' if result['passed'] else 'FAIL'}"
        )

    if args.enforce_thresholds and not overall_passed:
        print("")
        print("Threshold enforcement failed:")
        for scenario_key, result in scenario_results.items():
            if result["passed"]:
                continue
            stats = result["stats"]
            print(
                f"  - {scenario_key}: p99={stats['p99_ms']:.3f}ms >= "
                f"{result['threshold_p99_ms']:.3f}ms"
            )
        return 2

    if not args.keep_artifacts:
        # Keep artifacts always; only prune large run repos.
        runs_root = work_root / "runs"
        if runs_root.exists():
            shutil.rmtree(runs_root)

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as err:
        print(f"error: {err}", file=sys.stderr)
        raise SystemExit(1)
