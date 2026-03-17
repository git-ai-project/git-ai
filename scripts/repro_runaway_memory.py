#!/usr/bin/env python3
"""
Reproduce runaway memory patterns in git-ai.

This harness focuses on two high-risk paths:
1) Large working-log checkpoint replay (`checkpoints.jsonl` load/clone/rewrite path)
2) Large Claude transcript parsing (`checkpoint claude --hook-input` path)

It generates synthetic repos, runs git-ai commands, and samples peak RSS via `ps`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class RunMetrics:
    scenario: str
    attempt: int
    input_mb: float
    peak_rss_mb: float
    duration_s: float
    returncode: int
    artifact_dir: Path
    detail: dict[str, Any]


def run(
    cmd: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    capture: bool = False,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        env=env,
        check=True,
        text=True,
        capture_output=capture,
    )


def measure_peak_rss(
    cmd: list[str],
    *,
    cwd: Path,
    sample_interval_s: float,
) -> tuple[int, float, float, Path]:
    """
    Return (returncode, peak_rss_mb, duration_seconds, log_path).
    """
    cwd.mkdir(parents=True, exist_ok=True)
    log_path = cwd / "command.log"
    with log_path.open("w", encoding="utf-8") as log_file:
        started_at = time.time()
        proc = subprocess.Popen(
            cmd,
            cwd=str(cwd),
            stdout=log_file,
            stderr=log_file,
            text=True,
        )

        peak_kb = 0
        while True:
            if proc.poll() is not None:
                break
            try:
                rss_output = subprocess.check_output(
                    ["ps", "-o", "rss=", "-p", str(proc.pid)],
                    text=True,
                ).strip()
                if rss_output:
                    rss_kb = int(rss_output)
                    if rss_kb > peak_kb:
                        peak_kb = rss_kb
            except Exception:
                pass
            time.sleep(sample_interval_s)

        # One final sample after process exit.
        try:
            rss_output = subprocess.check_output(
                ["ps", "-o", "rss=", "-p", str(proc.pid)],
                text=True,
            ).strip()
            if rss_output:
                peak_kb = max(peak_kb, int(rss_output))
        except Exception:
            pass

        returncode = proc.wait()
        duration_s = time.time() - started_at

    return returncode, peak_kb / 1024.0, duration_s, log_path


def tail(path: Path, n: int = 40) -> str:
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except Exception:
        return ""
    if not lines:
        return ""
    return "\n".join(lines[-n:])


def make_repo(repo_dir: Path, file_count: int) -> str:
    run(["git", "init", "-q"], cwd=repo_dir)
    run(["git", "config", "user.name", "Repro Bot"], cwd=repo_dir)
    run(["git", "config", "user.email", "repro@example.com"], cwd=repo_dir)

    seed = "line0\nline1\nline2\n"
    for i in range(file_count):
        (repo_dir / f"f{i:05d}.txt").write_text(seed, encoding="utf-8")

    run(["git", "add", "."], cwd=repo_dir)
    run(["git", "commit", "-q", "-m", "seed"], cwd=repo_dir)
    return (
        run(["git", "rev-parse", "HEAD"], cwd=repo_dir, capture=True).stdout.strip()
    )


def write_synthetic_checkpoints(
    repo_dir: Path,
    head_sha: str,
    checkpoint_count: int,
    file_count: int,
    attrs_per_checkpoint: int,
) -> float:
    working_dir = repo_dir / ".git" / "ai" / "working_logs" / head_sha
    blobs_dir = working_dir / "blobs"
    blobs_dir.mkdir(parents=True, exist_ok=True)

    base_content = "line0\nline1\nline2\n"
    base_sha = hashlib.sha256(base_content.encode("utf-8")).hexdigest()
    (blobs_dir / base_sha).write_text(base_content, encoding="utf-8")

    attrs = [
        {
            "start": i * 2,
            "end": i * 2 + 1,
            "author_id": "synthetic-agent",
            "ts": 1700000000000,
        }
        for i in range(attrs_per_checkpoint)
    ]

    checkpoints_path = working_dir / "checkpoints.jsonl"
    with checkpoints_path.open("w", encoding="utf-8") as f:
        for i in range(checkpoint_count):
            file_path = f"f{i % file_count:05d}.txt"
            checkpoint = {
                "kind": "AiAgent",
                "diff": f"synthetic-{i}",
                "author": "synthetic",
                "entries": [
                    {
                        "file": file_path,
                        "blob_sha": base_sha,
                        "attributions": attrs,
                        "line_attributions": [],
                    }
                ],
                "timestamp": 1700000000 + i,
                "transcript": None,
                "agent_id": {
                    "tool": "mock_ai",
                    "id": f"session-{i:06d}",
                    "model": "synthetic",
                },
                "agent_metadata": None,
                "line_stats": {
                    "additions": 1,
                    "deletions": 0,
                    "additions_sloc": 1,
                    "deletions_sloc": 0,
                },
                "api_version": "checkpoint/1.0.0",
                "git_ai_version": "repro",
            }
            f.write(json.dumps(checkpoint, separators=(",", ":")))
            f.write("\n")

    target_file = repo_dir / "f00000.txt"
    target_file.write_text(base_content + "change\n", encoding="utf-8")
    return checkpoints_path.stat().st_size / (1024.0 * 1024.0)


def write_large_claude_transcript(
    repo_dir: Path,
    *,
    line_pairs: int,
    thinking_bytes: int,
) -> tuple[Path, float]:
    transcript_path = repo_dir / "claude-large.jsonl"
    thinking_blob = "x" * thinking_bytes

    with transcript_path.open("w", encoding="utf-8") as f:
        for i in range(line_pairs):
            user = {
                "type": "user",
                "timestamp": "2026-02-11T00:00:00Z",
                "message": {"content": f"user-message-{i}"},
            }
            assistant = {
                "type": "assistant",
                "timestamp": "2026-02-11T00:00:01Z",
                "message": {
                    "model": "claude-3-7-sonnet",
                    "content": [
                        {"type": "thinking", "thinking": thinking_blob},
                        {"type": "text", "text": "ack"},
                    ],
                },
            }
            f.write(json.dumps(user, separators=(",", ":")))
            f.write("\n")
            f.write(json.dumps(assistant, separators=(",", ":")))
            f.write("\n")

    # Ensure checkpoint path sees a changed file.
    (repo_dir / "f00000.txt").write_text("line0\nline1\nline2\nchanged\n", encoding="utf-8")
    size_mb = transcript_path.stat().st_size / (1024.0 * 1024.0)
    return transcript_path, size_mb


def run_checkpoint_repro(
    *,
    git_ai_bin: Path,
    root: Path,
    attempt: int,
    checkpoint_count: int,
    file_count: int,
    attrs_per_checkpoint: int,
    sample_interval_s: float,
) -> RunMetrics:
    scenario_dir = root / f"checkpoints_attempt_{attempt}"
    if scenario_dir.exists():
        shutil.rmtree(scenario_dir)
    scenario_dir.mkdir(parents=True, exist_ok=True)

    head = make_repo(scenario_dir, file_count)
    input_mb = write_synthetic_checkpoints(
        scenario_dir,
        head,
        checkpoint_count=checkpoint_count,
        file_count=file_count,
        attrs_per_checkpoint=attrs_per_checkpoint,
    )

    returncode, peak_rss_mb, duration_s, log_path = measure_peak_rss(
        [str(git_ai_bin), "checkpoint"],
        cwd=scenario_dir,
        sample_interval_s=sample_interval_s,
    )

    if returncode != 0:
        raise RuntimeError(
            "checkpoint scenario command failed\n"
            f"log tail:\n{tail(log_path)}"
        )

    return RunMetrics(
        scenario="checkpoints",
        attempt=attempt,
        input_mb=input_mb,
        peak_rss_mb=peak_rss_mb,
        duration_s=duration_s,
        returncode=returncode,
        artifact_dir=scenario_dir,
        detail={
            "checkpoint_count": checkpoint_count,
            "file_count": file_count,
            "attrs_per_checkpoint": attrs_per_checkpoint,
        },
    )


def run_claude_repro(
    *,
    git_ai_bin: Path,
    root: Path,
    attempt: int,
    line_pairs: int,
    thinking_bytes: int,
    sample_interval_s: float,
) -> RunMetrics:
    scenario_dir = root / f"claude_attempt_{attempt}"
    if scenario_dir.exists():
        shutil.rmtree(scenario_dir)
    scenario_dir.mkdir(parents=True, exist_ok=True)

    make_repo(scenario_dir, 1)
    transcript_path, input_mb = write_large_claude_transcript(
        scenario_dir,
        line_pairs=line_pairs,
        thinking_bytes=thinking_bytes,
    )
    hook_payload = {
        "transcript_path": str(transcript_path),
        "cwd": str(scenario_dir),
        "tool_input": {"file_path": "f00000.txt"},
        "hook_event_name": "PostToolUse",
    }

    returncode, peak_rss_mb, duration_s, log_path = measure_peak_rss(
        [
            str(git_ai_bin),
            "checkpoint",
            "claude",
            "--hook-input",
            json.dumps(hook_payload, separators=(",", ":")),
        ],
        cwd=scenario_dir,
        sample_interval_s=sample_interval_s,
    )

    if returncode != 0:
        raise RuntimeError(
            "claude scenario command failed\n"
            f"log tail:\n{tail(log_path)}"
        )

    return RunMetrics(
        scenario="claude",
        attempt=attempt,
        input_mb=input_mb,
        peak_rss_mb=peak_rss_mb,
        duration_s=duration_s,
        returncode=returncode,
        artifact_dir=scenario_dir,
        detail={
            "line_pairs": line_pairs,
            "thinking_bytes": thinking_bytes,
        },
    )


def print_metrics(metric: RunMetrics) -> None:
    ratio = metric.peak_rss_mb / metric.input_mb if metric.input_mb > 0 else 0.0
    print(
        f"[{metric.scenario}] attempt={metric.attempt} "
        f"input_mb={metric.input_mb:.2f} peak_rss_mb={metric.peak_rss_mb:.2f} "
        f"rss_to_input={ratio:.2f}x duration_s={metric.duration_s:.2f} detail={metric.detail}"
    )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Reproduce git-ai runaway memory behavior.",
    )
    parser.add_argument(
        "--git-ai-bin",
        default="target/debug/git-ai",
        help="Path to git-ai binary (default: target/debug/git-ai).",
    )
    parser.add_argument(
        "--scenario",
        choices=["checkpoints", "claude", "both"],
        default="both",
        help="Which scenario to run (default: both).",
    )
    parser.add_argument(
        "--work-root",
        default="/tmp/git-ai-memory-repro",
        help="Directory to store generated repro repos.",
    )
    parser.add_argument(
        "--keep-artifacts",
        action="store_true",
        help="Keep generated repos after completion.",
    )
    parser.add_argument(
        "--sample-interval-ms",
        type=int,
        default=25,
        help="RSS sample interval in milliseconds (default: 25).",
    )
    parser.add_argument(
        "--target-peak-mb",
        type=float,
        default=1024.0,
        help="Stop ramping a scenario once peak RSS reaches this MB (default: 1024).",
    )
    parser.add_argument(
        "--max-attempts",
        type=int,
        default=5,
        help="Maximum load-ramp attempts per scenario (default: 5).",
    )

    # Checkpoint scenario knobs (attempt 1 base values).
    parser.add_argument("--checkpoint-count", type=int, default=200)
    parser.add_argument("--checkpoint-files", type=int, default=400)
    parser.add_argument("--attrs-per-checkpoint", type=int, default=1500)

    # Claude scenario knobs (attempt 1 base values).
    parser.add_argument("--claude-line-pairs", type=int, default=1000)
    parser.add_argument("--claude-thinking-bytes", type=int, default=8000)

    args = parser.parse_args()

    git_ai_bin = Path(args.git_ai_bin).expanduser().resolve()
    if not git_ai_bin.exists():
        print(
            f"git-ai binary not found at {git_ai_bin}. "
            "Build first (example: cargo build --bin git-ai).",
            file=sys.stderr,
        )
        return 2

    work_root = Path(args.work_root).expanduser().resolve()
    if work_root.exists() and not args.keep_artifacts:
        shutil.rmtree(work_root)
    work_root.mkdir(parents=True, exist_ok=True)

    sample_interval_s = max(0.001, args.sample_interval_ms / 1000.0)
    all_metrics: list[RunMetrics] = []

    scenarios: list[str]
    if args.scenario == "both":
        scenarios = ["checkpoints", "claude"]
    else:
        scenarios = [args.scenario]

    try:
        for scenario in scenarios:
            print(f"\n=== Scenario: {scenario} ===")
            for attempt in range(1, args.max_attempts + 1):
                scale = 2 ** (attempt - 1)
                if scenario == "checkpoints":
                    metric = run_checkpoint_repro(
                        git_ai_bin=git_ai_bin,
                        root=work_root,
                        attempt=attempt,
                        checkpoint_count=args.checkpoint_count * scale,
                        file_count=max(args.checkpoint_files, args.checkpoint_count * scale),
                        attrs_per_checkpoint=args.attrs_per_checkpoint,
                        sample_interval_s=sample_interval_s,
                    )
                else:
                    metric = run_claude_repro(
                        git_ai_bin=git_ai_bin,
                        root=work_root,
                        attempt=attempt,
                        line_pairs=args.claude_line_pairs * scale,
                        thinking_bytes=args.claude_thinking_bytes,
                        sample_interval_s=sample_interval_s,
                    )

                all_metrics.append(metric)
                print_metrics(metric)

                if metric.peak_rss_mb >= args.target_peak_mb:
                    print(
                        f"Reached target peak RSS ({args.target_peak_mb:.0f} MB) "
                        f"for scenario '{scenario}' at attempt {attempt}."
                    )
                    break
            else:
                print(
                    f"Target peak RSS ({args.target_peak_mb:.0f} MB) was not reached for "
                    f"scenario '{scenario}' within {args.max_attempts} attempts."
                )

        summary = [
            {
                "scenario": m.scenario,
                "attempt": m.attempt,
                "input_mb": round(m.input_mb, 3),
                "peak_rss_mb": round(m.peak_rss_mb, 3),
                "duration_s": round(m.duration_s, 3),
                "detail": m.detail,
                "artifact_dir": str(m.artifact_dir),
            }
            for m in all_metrics
        ]
        print("\n=== Summary (JSON) ===")
        print(json.dumps(summary, indent=2))
    finally:
        if not args.keep_artifacts:
            shutil.rmtree(work_root, ignore_errors=True)

    return 0


if __name__ == "__main__":
    sys.exit(main())
